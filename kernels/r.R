#!/usr/bin/env Rscript
#
# Loom kernel for R.
#
# Implements the loom kernel protocol (kernels/PROTOCOL.md): the kernel
# connects to loom on 127.0.0.1, identifies itself with the token, then reads
# one JSON request per line and writes one JSON response per line until loom
# closes the connection. Needs the jsonlite and evaluate packages. SVG output
# uses svglite when it is installed (otherwise grDevices::svg), and plots are
# recorded on ragg's recording device when ragg is installed (otherwise on a
# null pdf device).
#
# Usage: Rscript r.R --connect P --token T --parent-pid PID --scratch DIR
#
# Every kernel object lives in a private environment whose parent is the base
# environment, so that user code (sessions inherit from the global
# environment) neither sees kernel internals nor can mask the functions the
# kernel calls. The only object visible to users is the loom_typst() generic,
# on the search path as "loom".

local({

`%||%` <- function(a, b) if (is.null(a)) b else a

for (pkg in c("jsonlite", "evaluate")) {
  if (!requireNamespace(pkg, quietly = TRUE)) {
    stop("the loom R kernel needs the '", pkg, "' package; install it with ",
         "install.packages(\"", pkg, "\")", call. = FALSE)
  }
}

state <- new.env(parent = emptyenv())
state$sessions <- new.env(parent = emptyenv())
state$images <- 0L
state$noted_fallback <- character()
state$priorities <- new.env(parent = emptyenv())
state$shared <- 0L
state$has_svglite <- requireNamespace("svglite", quietly = TRUE)
state$has_ragg <- requireNamespace("ragg", quietly = TRUE) &&
  exists("agg_record", envir = asNamespace("ragg"), inherits = FALSE)

kernel_log <- function(...) {
  cat(format(Sys.time(), "%Y-%m-%d %H:%M:%OS3"), " r kernel: ", ..., "\n",
      sep = "", file = stderr())
}

# ---------------------------------------------------------------------------
# loom_typst() generic

typst_env <- new.env(parent = baseenv())
# topenv() treats an environment that holds .packageName as top level. That
# makes UseMethod() consult the S3 methods table that registerS3method()
# creates in this environment, so methods registered here (or by users with
# .S3method()) are found from every session.
typst_env$.packageName <- "loom"

loom_typst <- function(x, ...) UseMethod("loom_typst")
environment(loom_typst) <- typst_env
typst_env$loom_typst <- loom_typst

registerS3method("loom_typst", "default", function(x, ...) NULL,
                 envir = typst_env)
registerS3method("loom_typst", "tinytable", function(x, ...) {
  tinytable::save_tt(x, output = "typst")
}, envir = typst_env)
registerS3method("loom_typst", "gt_tbl", function(x, ...) {
  as_typst <- get0("as_typst", envir = asNamespace("gt"), inherits = FALSE)
  if (!is.function(as_typst)) {
    stop("gt ", format(utils::packageVersion("gt")), " cannot export Typst ",
         "(it has no as_typst()); use tinytable, or define a ",
         "loom_typst.gt_tbl() method", call. = FALSE)
  }
  paste(as.character(as_typst(x)), collapse = "\n")
}, envir = typst_env)

assign("loom_typst", loom_typst,
       envir = attach(NULL, pos = 2L, name = "loom"))

# ---------------------------------------------------------------------------
# Running user code

# All user code runs through one of these two functions or through
# evaluate::evaluate(). user_calls() uses them to find where user frames
# begin, and the eval() calls are written as eval(expr, envir) so that
# drop_eval_call() removes them from condition calls as evaluate does.
eval_user <- function(expr, envir) eval(expr, envir)

# Calls `call`, which refers to the value as `x`, from a child of the session
# environment, as R's top level does when it prints a value. S3 dispatch then
# finds methods (print.myclass, loom_typst.myclass) defined in the session.
# `loom_typst` is bound in that child so that it names the kernel's generic
# even when the session defines an object of the same name.
call_in_session <- function(call, value, env) {
  envir <- new.env(parent = env)
  envir$x <- value
  envir$loom_typst <- loom_typst
  expr <- call
  eval(expr, envir)
}

drop_eval_call <- function(call) {
  if (identical(call, quote(eval(expr, envir)))) NULL else call
}

# Called from an error calling handler: returns the calls of the user frames
# between the kernel's eval of user code and the frame that signaled the
# error, outermost first.
user_calls <- function() {
  handler <- sys.parent()
  calls <- sys.calls()
  parents <- sys.parents()
  owners <- list(evaluate::evaluate, eval_user, call_in_session)
  is_owner <- function(i) {
    f <- sys.function(i)
    any(vapply(owners, identical, logical(1), f))
  }
  start <- NA_integer_
  for (j in seq_len(handler - 1L)) {
    p <- parents[[j]]
    if (p > 0L && identical(sys.function(j), base::eval) && is_owner(p)) {
      start <- j
      break
    }
  }
  if (is.na(start)) return(list())
  # eval() shows up twice: the closure and the context of its .Internal.
  first <- start + 1L
  while (first < handler && identical(calls[[first]], calls[[start]])) {
    first <- first + 1L
  }
  if (first >= handler) return(list())
  idx <- seq.int(first, handler - 1L)
  internal <- vapply(idx, function(i) {
    identical(sys.function(i), base::.handleSimpleError)
  }, logical(1))
  calls[idx[!internal]]
}

# Text captured from user code can hold bytes that are not valid UTF-8, on
# which regular expressions and nchar() fail. Such bytes are shown as <xx>.
utf8 <- function(s) {
  s <- enc2utf8(s)
  bad <- !validUTF8(s)
  if (any(bad)) s[bad] <- iconv(s[bad], "UTF-8", "UTF-8", sub = "byte")
  s
}

deparse_call <- function(call) {
  text <- utf8(deparse(call, width.cutoff = 500L, nlines = 1L))
  if (nchar(text) > 100L) paste0(substr(text, 1L, 97L), "...") else text
}

trim_newlines <- function(text) sub("\n+$", "", utf8(text))

warning_text <- function(cnd) {
  call <- drop_eval_call(conditionCall(cnd))
  msg <- trim_newlines(conditionMessage(cnd))
  if (is.null(call)) paste0("Warning: ", msg)
  else paste0("Warning in ", deparse_call(call), ": ", msg)
}

traceback_lines <- function(calls) {
  n <- length(calls)
  if (n == 0L) return(character())
  lines <- rev(sprintf("%d: %s", seq_len(n), vapply(calls, deparse_call, "")))
  if (n > 10L) lines <- c(lines[1:5], "...", lines[(n - 4L):n])
  c("Traceback:", paste0("  ", lines))
}

error_text <- function(cnd, calls = list(), where = NULL) {
  call <- drop_eval_call(conditionCall(cnd))
  msg <- trim_newlines(conditionMessage(cnd))
  head <- if (is.null(call)) paste0("Error: ", msg)
          else paste0("Error in ", deparse_call(call), ": ", msg)
  paste(c(head, if (!is.null(where)) paste0("  at ", where),
          traceback_lines(calls)), collapse = "\n")
}

# An interrupted request ends with an error output whose text starts with
# "interrupted".
interrupted_text <- function(where = NULL) {
  paste(c("interrupted", if (!is.null(where)) paste0("  at ", where)),
        collapse = "\n")
}

# Marks an interrupt caught around user code, distinct from user errors.
interrupted <- function() {
  structure(class = c("loom_interrupted", "condition"),
            list(message = "interrupted", call = NULL))
}

# `where` holds the optional `file` and `line` of the request; `cell_line` is
# a 1-based line within the cell.
location <- function(where, cell_line) {
  line <- if (!is.null(where$line)) where$line + cell_line - 1L
  if (!is.null(where$file) && !is.null(line)) paste0(where$file, ":", line)
  else if (!is.null(line)) paste0("line ", line)
  else if (!is.null(where$file)) {
    paste0(where$file, ", line ", cell_line, " of the cell")
  } else paste0("line ", cell_line, " of the cell")
}

# Splits on "\n" only, keeping a trailing empty line, so that
# paste(split_lines(code), collapse = "\n") is identical to code.
split_lines <- function(code) {
  lines <- strsplit(code, "\n", fixed = TRUE)[[1L]]
  if (!nzchar(code) || endsWith(code, "\n")) lines <- c(lines, "")
  lines
}

# Parses the lines of a cell. On a syntax error, raises a loom_parse_error
# whose message gives positions in the document when the request carries
# them.
parse_lines <- function(lines, where) {
  srcfile <- srcfilecopy("<text>", lines)
  exprs <- tryCatch(parse(text = lines, srcfile = srcfile, keep.source = TRUE),
                    error = identity)
  if (!inherits(exprs, "error")) return(exprs)
  msg <- conditionMessage(exprs)
  if (!is.null(where$line)) {
    file <- gsub("([\"\\\\])", "\\\\\\1", where$file %||% "<text>")
    directive <- sprintf("#line %d \"%s\"", as.integer(where$line), file)
    again <- tryCatch(parse(text = c(directive, lines), keep.source = TRUE),
                      error = identity)
    if (inherits(again, "error")) msg <- conditionMessage(again)
  } else if (!is.null(where$file)) {
    msg <- sub("^<text>", where$file, msg)
  }
  stop(structure(class = c("loom_parse_error", "error", "condition"),
                 list(message = msg, call = NULL)))
}

# Top-level statements grouped as evaluate::evaluate() groups them
# (statements that share a line form one group). `first` and `last` are the
# lines spanned by each group's statements; `chunk` is the text of the source
# output for the group: its own lines plus the statement-free lines before
# it, and for the last group also the statement-free lines after it.
statement_groups <- function(exprs, lines) {
  srcref <- attr(exprs, "srcref")
  if (length(srcref) == 0L) return(NULL)
  # Elements 7 and 8 are the parsed lines, unaffected by #line directives;
  # evaluate uses the same ones.
  first <- vapply(srcref, function(r) as.integer(r[[7L]]), integer(1))
  last <- vapply(srcref, function(r) as.integer(r[[8L]]), integer(1))
  starts <- c(TRUE, first[-1L] != last[-length(last)])
  group <- cumsum(starts)
  g_first <- first[starts]
  g_last <- vapply(split(last, group), max, integer(1), USE.NAMES = FALSE)
  n <- length(g_first)
  from <- c(1L, g_last[-n] + 1L)
  to <- c(g_last[-n], length(lines))
  chunk <- vapply(seq_len(n), function(k) {
    paste(lines[from[k]:to[k]], collapse = "\n")
  }, "")
  list(first = g_first, last = g_last, chunk = chunk, n = n)
}

# ---------------------------------------------------------------------------
# Values

print_text <- function(value, env) {
  call <- if (isS4(value)) quote(methods::show(x)) else quote(print(x))
  paste(utils::capture.output(call_in_session(call, value, env)),
        collapse = "\n")
}

typst_text <- function(value, env) {
  out <- call_in_session(quote(loom_typst(x)), value, env)
  if (is.null(out)) return(NULL)
  if (!is.character(out)) {
    stop("loom_typst() returned an object of class '", class(out)[[1L]],
         "'; it must return a character string or NULL", call. = FALSE)
  }
  paste(out, collapse = "\n")
}

# The `value` output for a visible top-level value, or NULL when there is
# none to emit: print() wrote no text (a ggplot object, whose drawing is
# captured as a plot) and no Typst markup is available.
value_output <- function(value, env) {
  text <- print_text(value, env)
  typst <- typst_text(value, env)
  if (is.null(typst) && !nzchar(text)) return(NULL)
  data <- list(`text/plain` = text)
  if (is.character(value)) {
    data[["text/x-string"]] <- paste(value, collapse = "\n")
  }
  if (!is.null(typst)) data[["text/typst"]] <- typst
  list(type = "value", repl_only = FALSE, data = data)
}

inline_output <- function(value, env) {
  pieces <- if (is.atomic(value) && !is.object(value)) {
    format(value, trim = TRUE, justify = "none")
  } else {
    call_in_session(quote(format(x)), value, env)
  }
  data <- list(`text/plain` = paste(pieces, collapse = ", "))
  typst <- typst_text(value, env)
  if (!is.null(typst)) data[["text/typst"]] <- typst
  list(type = "value", repl_only = FALSE, data = data)
}

# ---------------------------------------------------------------------------
# Graphics

mime_types <- c(svg = "image/svg+xml", png = "image/png",
                pdf = "application/pdf")

null_device <- function(width = 7, height = 7, ...) {
  grDevices::pdf(file = NULL, width = width, height = height)
}

close_device <- function(dev) {
  if (dev %in% grDevices::dev.list()) grDevices::dev.off(dev)
  invisible()
}

open_recording_device <- function(opts) {
  if (state$has_ragg) {
    ragg::agg_record(width = opts$width, height = opts$height, units = "in")
  } else {
    grDevices::pdf(file = NULL, width = opts$width, height = opts$height)
  }
  grDevices::dev.control(displaylist = "enable")
  grDevices::dev.cur()
}

format_available <- function(fmt) {
  switch(fmt,
    svg = state$has_svglite || isTRUE(capabilities("cairo")[[1L]]),
    png = isTRUE(capabilities("png")[[1L]]),
    pdf = TRUE)
}

pick_format <- function(requested) {
  for (fmt in unique(c(requested, "svg", "png", "pdf"))) {
    if (format_available(fmt)) {
      if (fmt != requested && !requested %in% state$noted_fallback) {
        kernel_log("no ", requested, " device is available; saving plots as ",
                   fmt)
        state$noted_fallback <- c(state$noted_fallback, requested)
      }
      return(fmt)
    }
  }
}

replay_to_file <- function(plot, fmt, path, opts) {
  w <- opts$width
  h <- opts$height
  switch(fmt,
    svg = if (state$has_svglite) {
      svglite::svglite(path, width = w, height = h)
    } else {
      grDevices::svg(path, width = w, height = h)
    },
    png = grDevices::png(path, width = w, height = h, units = "in",
                         res = opts$dpi),
    pdf = grDevices::pdf(path, width = w, height = h))
  dev <- grDevices::dev.cur()
  on.exit(close_device(dev))
  grDevices::replayPlot(plot)
}

# Replays a recorded plot into a new file in the scratch directory. Warnings
# raised by the output device (missing glyphs, for instance) are returned so
# that they appear next to the figure.
save_plot <- function(plot, opts) {
  fmt <- pick_format(opts$format)
  dir.create(state$scratch, recursive = TRUE, showWarnings = FALSE)
  state$images <- state$images + 1L
  path <- file.path(state$scratch, sprintf("loom-r-%d-%d.%s", Sys.getpid(),
                                           state$images, fmt))
  warnings <- character()
  result <- tryCatch(
    withCallingHandlers(replay_to_file(plot, fmt, path, opts),
      warning = function(w) {
        warnings <<- c(warnings, warning_text(w))
        invokeRestart("muffleWarning")
      }),
    error = identity)
  if (inherits(result, "error")) {
    stop("could not save a plot as ", fmt, ": ", conditionMessage(result),
         call. = FALSE)
  }
  if (!file.exists(path)) {
    stop("the ", fmt, " device wrote no file at ", path, call. = FALSE)
  }
  list(mime = mime_types[[fmt]], path = path, warnings = warnings)
}

# Display lists are pairlists and `[` returns a list, hence a[].
display_list_is_prefix <- function(a, b) {
  length(a) <= length(b) && identical(a[], b[seq_along(a)])
}

# A plot whose display list is a prefix of the next plot's was only an
# intermediate state of that plot (for instance plot() followed by
# abline()); it is dropped, as knitr does by default.
drop_intermediate_plots <- function(events) {
  idx <- which(vapply(events, is_type, logical(1), "plot"))
  drop <- integer()
  for (k in seq_along(idx)[-1L]) {
    if (display_list_is_prefix(events[[idx[k - 1L]]]$plot[[1L]],
                               events[[idx[k]]]$plot[[1L]])) {
      drop <- c(drop, idx[k - 1L])
    }
  }
  if (length(drop)) events[-drop] else events
}

render_plots <- function(events, opts) {
  out <- list()
  for (ev in events) {
    if (identical(ev$type, "plot")) {
      saved <- save_plot(ev$plot, opts)
      for (w in saved$warnings) {
        out[[length(out) + 1L]] <- list(type = "warning", text = w)
      }
      data <- structure(list(list(file = saved$path)), names = saved$mime)
      out[[length(out) + 1L]] <- list(type = "display", data = data)
    } else {
      out[[length(out) + 1L]] <- ev
    }
  }
  out
}

# ---------------------------------------------------------------------------
# Output post-processing

is_type <- function(ev, type) identical(ev$type, type)

merge_streams <- function(events) {
  out <- list()
  for (ev in events) {
    n <- length(out)
    if (is_type(ev, "stream") && n > 0L && is_type(out[[n]], "stream") &&
        identical(out[[n]]$name, ev$name)) {
      out[[n]]$text <- paste0(out[[n]]$text, ev$text)
    } else {
      out[[n + 1L]] <- ev
    }
  }
  out
}

# Puts the error output last, merges consecutive stream outputs with the same
# name, drops stream outputs that hold only newlines and strips trailing
# newlines from stream text. Stream text stays raw until the last step, so
# that merging the neighbors of a dropped output keeps their line breaks.
finish_outputs <- function(events) {
  errors <- vapply(events, is_type, logical(1), "error")
  events <- merge_streams(c(events[!errors], events[errors]))
  blank <- vapply(events, function(ev) {
    is_type(ev, "stream") && !nzchar(trim_newlines(ev$text))
  }, logical(1))
  lapply(merge_streams(events[!blank]), function(ev) {
    if (is_type(ev, "stream")) ev$text <- trim_newlines(ev$text)
    ev
  })
}

read_available <- function(con, size = 65536L) {
  bytes <- raw()
  repeat {
    chunk <- readBin(con, "raw", n = size)
    bytes <- c(bytes, chunk)
    if (length(chunk) < size) break
  }
  bytes
}

# ---------------------------------------------------------------------------
# Execution modes

exec_block <- function(code, env, opts, where) {
  lines <- split_lines(code)
  exprs <- tryCatch(parse_lines(lines, where), error = identity)
  if (inherits(exprs, "error")) {
    return(list(failed = TRUE, outputs = list(
      list(type = "source", code = code),
      list(type = "error", text = paste0("Error: ", conditionMessage(exprs))))))
  }
  groups <- statement_groups(exprs, lines)
  if (is.null(groups)) {
    outputs <- if (nzchar(code)) list(list(type = "source", code = code))
    return(list(failed = FALSE, outputs = outputs %||% list()))
  }

  st <- new.env(parent = emptyenv())
  st$events <- list()
  st$group <- 0L
  st$failed <- FALSE
  st$calls <- list()
  st$err_con <- NULL

  push <- function(ev) st$events[[length(st$events) + 1L]] <- ev
  # Text written to stderr is collected through a message sink and emitted
  # before the next output, which is as close to its real position as R
  # allows (stdout and stderr are captured separately).
  drain_stderr <- function() {
    if (is.null(st$err_con)) return(invisible())
    text <- rawToChar(read_available(st$err_con))
    if (nzchar(text)) push(list(type = "stream", name = "stderr", text = text))
    invisible()
  }
  add <- function(ev) {
    drain_stderr()
    push(ev)
  }
  where_now <- function() location(where, groups$first[[max(st$group, 1L)]])

  handler <- evaluate::new_output_handler(
    source = function(src, exprs) {
      if (length(exprs) == 0L) return(NULL)
      k <- st$group + 1L
      expected <- if (k <= groups$n) {
        paste0(paste(lines[groups$first[[k]]:groups$last[[k]]],
                     collapse = "\n"), "\n")
      }
      if (is.null(expected) || src != expected) {
        stop("evaluate split the cell differently from the kernel at ",
             "statement group ", k, call. = FALSE)
      }
      st$group <- k
      add(list(type = "source", code = groups$chunk[[k]]))
      NULL
    },
    text = function(o) add(list(type = "stream", name = "stdout", text = o)),
    graphics = function(p) add(list(type = "plot", plot = p)),
    message = function(m) {
      add(list(type = "message", text = trim_newlines(conditionMessage(m))))
    },
    warning = function(w) add(list(type = "warning", text = warning_text(w))),
    error = function(e) {
      st$failed <- TRUE
      add(list(type = "error", text = error_text(e, st$calls, where_now())))
    },
    value = function(value, visible, envir) {
      if (visible) {
        out <- value_output(value, env)
        if (!is.null(out)) add(out)
      }
      invisible()
    },
    calling_handlers = list(error = function(e) st$calls <- user_calls()))

  rec_dev <- open_recording_device(opts)
  on.exit(close_device(rec_dev), add = TRUE)
  # evaluate() sets this option only when it is unset and never restores it.
  old_options <- options(rlang_trace_top_env = env)
  on.exit(options(old_options), add = TRUE)
  st$err_con <- file("", "w+b")
  sink(st$err_con, type = "message")
  on.exit({
    if (!is.null(st$err_con)) {
      sink(type = "message")
      close(st$err_con)
    }
  }, add = TRUE)

  tryCatch(
    allowInterrupts(evaluate::evaluate(
      lines, envir = env, stop_on_error = 1L, new_device = FALSE,
      output_handler = handler, keep_warning = TRUE, keep_message = TRUE)),
    interrupt = function(cnd) {
      st$failed <- TRUE
      add(list(type = "error", text = interrupted_text(where_now())))
    })

  drain_stderr()
  sink(type = "message")
  close(st$err_con)
  st$err_con <- NULL

  if (!st$failed && st$group != groups$n) {
    stop("evaluate ran ", st$group, " statement groups; the cell has ",
         groups$n, call. = FALSE)
  }
  events <- render_plots(drop_intermediate_plots(st$events), opts)
  list(failed = st$failed, outputs = finish_outputs(events))
}

exec_inline <- function(code, env, where) {
  calls <- list()
  result <- tryCatch(
    allowInterrupts(withCallingHandlers({
      exprs <- parse_lines(split_lines(code), where)
      if (length(exprs) != 1L) {
        stop("inline code must be a single expression; found ",
             length(exprs), call. = FALSE)
      }
      inline_output(eval_user(exprs[[1L]], env), env)
    }, error = function(e) calls <<- user_calls())),
    error = identity,
    interrupt = function(cnd) interrupted())
  if (inherits(result, "loom_interrupted")) {
    text <- interrupted_text(location(where, 1L))
    return(list(failed = TRUE, outputs = list(list(type = "error", text = text))))
  }
  if (inherits(result, "error")) {
    where <- if (!inherits(result, "loom_parse_error")) location(where, 1L)
    text <- error_text(result, calls, where)
    return(list(failed = TRUE, outputs = list(list(type = "error", text = text))))
  }
  list(failed = FALSE, outputs = list(result))
}

# Runs setup strings without capturing output. Plots go to a null device.
# Returns NULL on success, otherwise the text of the error output.
run_setup <- function(env, setup) {
  null_device()
  dev <- grDevices::dev.cur()
  on.exit(close_device(dev))
  for (i in seq_along(setup)) {
    calls <- list()
    result <- tryCatch(
      allowInterrupts(withCallingHandlers({
        exprs <- parse(text = setup[[i]], keep.source = FALSE)
        for (expr in exprs) eval_user(expr, env)
        NULL
      }, error = function(e) calls <<- user_calls())),
      error = identity,
      interrupt = function(cnd) interrupted())
    if (inherits(result, "loom_interrupted")) {
      return(interrupted_text(sprintf("setup string %d", i)))
    }
    if (inherits(result, "error")) {
      return(error_text(result, calls, sprintf("setup string %d", i)))
    }
  }
  NULL
}

# ---------------------------------------------------------------------------
# Session state and loaded packages

# Names R creates on its own; never reported as changed.
unreported_names <- c(".Random.seed", "...")

# What a name is bound to, read without side effects: the function of an
# active binding (not its value), and for a promise its expression (forcing
# it could run arbitrary code). substitute() returns an ordinary binding's
# object itself, without copying.
binding_of <- function(name, env) {
  if (bindingIsActive(name, env)) {
    list(TRUE, activeBindingFunction(name, env))
  } else {
    list(FALSE, eval(call("substitute", as.name(name), env)))
  }
}

binding_snapshot <- function(env) {
  names <- setdiff(ls(env, all.names = TRUE, sorted = FALSE), unreported_names)
  snapshot <- vector("list", length(names))
  names(snapshot) <- names
  for (name in names) snapshot[[name]] <- binding_of(name, env)
  snapshot
}

# Base R cannot test whether two objects are the same object, except for
# environments and external pointers, which identical() compares by
# reference. Other objects are compared by value with the strictest
# identical(), so rebinding a name to an equal copy is not reported. The
# snapshot holds references, so the pointer check at the start of identical()
# settles unchanged bindings without comparing contents; it also means that a
# vector modified in place by the request (x[1] <- 0) is copied first.
same_binding <- function(a, b) {
  identical(a[[1L]], b[[1L]]) &&
    identical(a[[2L]], b[[2L]], num.eq = FALSE, single.NA = FALSE,
              attrib.as.set = FALSE, ignore.bytecode = FALSE,
              ignore.environment = FALSE, ignore.srcref = FALSE)
}

changed_names <- function(before, after) {
  common <- intersect(names(before), names(after))
  same <- vapply(common, function(name) {
    same_binding(before[[name]], after[[name]])
  }, logical(1))
  sort(c(setdiff(names(after), names(before)),
         setdiff(names(before), names(after)), common[!same]),
       method = "radix")
}

package_priority <- function(path) {
  priority <- get0(path, envir = state$priorities, inherits = FALSE)
  if (is.null(priority)) {
    priority <- unname(read.dcf(file.path(path, "DESCRIPTION"),
                                fields = "Priority")[1L, 1L])
    if (is.na(priority)) priority <- ""
    assign(path, priority, envir = state$priorities)
  }
  priority
}

# Loaded namespaces other than R's base packages. A package loaded from
# source by pkgload (devtools::load_all) is reported as the source directory,
# since editing its R files does not change its DESCRIPTION.
loaded_packages <- function() {
  out <- list()
  for (ns in sort(loadedNamespaces(), method = "radix")) {
    # getNamespaceInfo() refuses the base namespace.
    path <- if (identical(ns, "base")) system.file(package = "base")
            else getNamespaceInfo(ns, "path")
    if (identical(package_priority(path), "base")) next
    from_source <- exists(".__DEVTOOLS__", envir = asNamespace(ns),
                          inherits = FALSE)
    out[[length(out) + 1L]] <- list(
      name = ns,
      version = unname(getNamespaceVersion(ns)),
      kind = if (from_source) "tree" else "file",
      path = normalizePath(if (from_source) path
                           else file.path(path, "DESCRIPTION"),
                           mustWork = TRUE))
  }
  out
}

# ---------------------------------------------------------------------------
# Shared data: values passed between sessions and languages as Arrow IPC
# streams, read and written with nanoarrow (see "Shared data" in
# kernels/PROTOCOL.md).

atomic_types <- c("integer", "double", "logical", "character")

shareable_values <- paste(
  "data frames (columns of integers, doubles, logicals, strings, factors,",
  "Dates and POSIXct date-times), atomic vectors, matrices and arrays",
  "(integer, double, logical or character, without NA) and single values",
  "of those types")

share_error <- function(...) stop(simpleError(paste0(...)))

need_nanoarrow <- function() {
  if (!requireNamespace("nanoarrow", quietly = TRUE)) {
    share_error("the R package 'nanoarrow' is not installed; install it ",
                "with install.packages(\"nanoarrow\")")
  }
}

next_shared_path <- function() {
  dir.create(state$scratch, recursive = TRUE, showWarnings = FALSE)
  state$shared <- state$shared + 1L
  file.path(state$scratch, sprintf("loom-r-%d-data-%d.arrow", Sys.getpid(),
                                   state$shared))
}

# NaN is a number, not a missing value.
has_missing <- function(x) {
  if (is.double(x)) any(is.na(x) & !is.nan(x)) else anyNA(x)
}

describe_value <- function(x) {
  cls <- oldClass(x)
  if (!is.null(cls)) {
    return(sprintf("an object of class '%s'", paste(cls, collapse = "/")))
  }
  switch(typeof(x),
    closure = , builtin = , special = "a function",
    list = "a list",
    environment = "an environment",
    NULL = "NULL",
    paste0("a value of type '", typeof(x), "'"))
}

# The kind of a data frame column that can be shared, or NA.
column_kind <- function(col) {
  if (is.factor(col)) "factor"
  else if (inherits(col, "Date")) "Date"
  else if (inherits(col, "POSIXct")) "POSIXct"
  else if (!is.object(col) && is.null(dim(col)) && typeof(col) %in% atomic_types) {
    typeof(col)
  } else NA_character_
}

# The table to write for `x`, with its loom.kind and, for an array, its
# shape.
shared_table <- function(x) {
  if (is.data.frame(x)) {
    table <- as.data.frame(x)
    for (name in names(table)) {
      col <- table[[name]]
      kind <- column_kind(col)
      if (is.na(kind)) {
        share_error("column '", name, "' is ", describe_value(col), "; data ",
                    "frame columns can hold integers, doubles, logicals, ",
                    "strings, factors, Dates and POSIXct date-times")
      }
      # nanoarrow cannot write dictionary arrays, so a factor is written as
      # its labels.
      if (kind == "factor") table[[name]] <- as.character(col)
      if (kind == "character") table[[name]] <- enc2utf8(col)
    }
    row.names(table) <- NULL
    return(list(kind = "dataframe", table = table))
  }
  if (is.atomic(x) && typeof(x) %in% atomic_types &&
      all(oldClass(x) %in% c("matrix", "array"))) {
    if (has_missing(x)) {
      share_error("it contains missing values (NA), which only data frame ",
                  "columns can hold")
    }
    values <- as.vector(x)
    if (is.character(values)) values <- enc2utf8(values)
    if (is.null(dim(x)) && length(x) == 1L) {
      return(list(kind = "scalar",
                  table = data.frame(value = values, stringsAsFactors = FALSE)))
    }
    shape <- if (is.null(dim(x))) length(x) else dim(x)
    return(list(kind = "array",
                table = data.frame(values = values, stringsAsFactors = FALSE),
                shape = shape))
  }
  share_error("it is ", describe_value(x), "; only these values can be ",
              "shared: ", shareable_values)
}

# A float64 array in which only NA_real_ is null: nanoarrow would also turn
# NaN into null, and NaN is a number. The data buffer is R's own vector; the
# validity bitmap is the data buffer of a bool array.
double_array <- function(x) {
  missing <- is.na(x) & !is.nan(x)
  validity <- if (any(missing)) {
    nanoarrow::as_nanoarrow_array(!missing)$buffers[[2L]]
  }
  nanoarrow::nanoarrow_array_modify(
    nanoarrow::nanoarrow_array_init(nanoarrow::na_double()),
    list(length = length(x), null_count = sum(missing),
         buffers = list(validity, x)))
}

write_shared <- function(x, path) {
  shared <- shared_table(x)
  metadata <- list(loom.kind = shared$kind)
  if (!is.null(shared$shape)) {
    metadata[["loom.shape"]] <-
      as.character(jsonlite::toJSON(as.integer(shared$shape)))
  }
  schema <- nanoarrow::nanoarrow_schema_modify(
    nanoarrow::infer_nanoarrow_schema(shared$table), list(metadata = metadata))
  array <- nanoarrow::as_nanoarrow_array(shared$table, schema = schema)
  doubles <- vapply(shared$table, function(col) is.double(col) && !is.object(col),
                    logical(1))
  if (any(doubles)) {
    columns <- array$children
    for (i in which(doubles)) columns[[i]] <- double_array(shared$table[[i]])
    array <- nanoarrow::nanoarrow_array_modify(array, list(children = columns))
  }
  nanoarrow::write_nanoarrow(array, path)
}

# Arrow integer types that nanoarrow converts to double (int64, uint32,
# uint64). Columns whose values all fit in R's integer range become integer.
wide_integer_formats <- c("l", "I", "L")

narrow_integers <- function(x) {
  fits <- is.na(x) | (x == trunc(x) & abs(x) <= .Machine$integer.max)
  if (all(fits)) as.integer(x) else x
}

# Arrow types of columns that can be shared (format strings of the Arrow C
# data interface): integers, floats, booleans, strings, date32/date64 and
# timestamps; a dictionary must have string values.
shareable_column <- function(column) {
  if (!is.null(column$dictionary)) {
    return(column$dictionary$format %in% c("u", "U"))
  }
  column$format %in% c("c", "C", "s", "S", "i", "I", "l", "L", "f", "g", "b",
                       "u", "U", "tdD", "tdm") ||
    startsWith(column$format, "ts")
}

arrow_type <- function(column) sub("^<nanoarrow_schema (.*)>$", "\\1", format(column))

imported_array <- function(data, shape_json) {
  if (!identical(names(data), "values")) {
    share_error("an array must be stored as one column named 'values'")
  }
  if (is.null(shape_json)) share_error("the file has no 'loom.shape' metadata")
  shape <- tryCatch(jsonlite::parse_json(shape_json, simplifyVector = TRUE),
                    error = function(e) NULL)
  if (!is.numeric(shape) || length(shape) == 0L || anyNA(shape) ||
      any(shape < 0) || any(shape != trunc(shape))) {
    share_error("invalid loom.shape '", shape_json, "'")
  }
  values <- data$values
  if (!column_kind(values) %in% atomic_types) {
    share_error("array elements must be numbers, booleans or strings, not ",
                describe_value(values))
  }
  if (has_missing(values)) share_error("the array contains missing values")
  if (length(values) != prod(shape)) {
    share_error("loom.shape ", shape_json, " needs ", prod(shape),
                " elements; the file holds ", length(values))
  }
  if (length(shape) == 1L) values else array(values, dim = as.integer(shape))
}

imported_scalar <- function(data) {
  if (!identical(names(data), "value") || nrow(data) != 1L) {
    share_error("a scalar must be stored as one column named 'value' with ",
                "one row")
  }
  value <- data$value
  if (!column_kind(value) %in% atomic_types) {
    share_error("a scalar must be a number, a boolean or a string, not ",
                describe_value(value))
  }
  if (has_missing(value)) share_error("the scalar is missing")
  value
}

read_shared <- function(file) {
  if (!file.exists(file)) share_error("the file does not exist")
  stream <- nanoarrow::read_nanoarrow(file)
  on.exit(stream$release())
  schema <- stream$get_schema()
  kind <- schema$metadata[["loom.kind"]]
  if (is.null(kind)) share_error("the file has no 'loom.kind' metadata")
  if (!kind %in% c("dataframe", "array", "scalar")) {
    share_error("unknown loom.kind '", kind, "'")
  }
  columns <- schema$children
  for (column in columns) {
    if (!shareable_column(column) ||
        (kind != "dataframe" && !is.null(column$dictionary))) {
      share_error("column '", column$name, "' has Arrow type ",
                  arrow_type(column), ", which cannot be shared")
    }
  }
  batches <- nanoarrow::collect_array_stream(stream, validate = FALSE)
  dictionary <- vapply(columns, function(ch) !is.null(ch$dictionary),
                       logical(1))
  # nanoarrow cannot convert a dictionary flagged as ordered: the flag is
  # cleared for the conversion and the factor made ordered afterwards.
  ordered <- dictionary &
    vapply(columns, function(ch) bitwAnd(ch$flags, 1L) != 0L, logical(1))
  if (any(ordered)) {
    for (i in which(ordered)) {
      columns[[i]] <- nanoarrow::nanoarrow_schema_modify(
        columns[[i]], list(flags = bitwAnd(columns[[i]]$flags, bitwNot(1L))),
        validate = FALSE)
    }
    schema <- nanoarrow::nanoarrow_schema_modify(
      schema, list(children = columns), validate = FALSE)
    batches <- lapply(batches, nanoarrow::nanoarrow_array_set_schema,
                      schema = schema, validate = FALSE)
  }
  ptype <- nanoarrow::infer_nanoarrow_ptype(schema)
  for (i in which(dictionary)) ptype[[i]] <- factor()
  # nanoarrow warns when a value cannot be represented exactly in R.
  data <- withCallingHandlers(
    nanoarrow::convert_array_stream(
      nanoarrow::basic_array_stream(batches, schema = schema, validate = FALSE),
      to = ptype),
    warning = function(w) share_error(conditionMessage(w)))
  formats <- vapply(columns, function(ch) ch$format, "")
  for (i in which(formats %in% wide_integer_formats & !dictionary)) {
    data[[i]] <- narrow_integers(data[[i]])
  }
  for (i in which(ordered)) {
    data[[i]] <- factor(data[[i]], levels = levels(data[[i]]), ordered = TRUE)
  }
  switch(kind,
    dataframe = data,
    array = imported_array(data, schema$metadata[["loom.shape"]]),
    scalar = imported_scalar(data))
}

# Returns list(values = <named list>) or list(error = <error output text>).
import_values <- function(imports) {
  values <- list()
  for (imp in imports) {
    value <- tryCatch({
      need_nanoarrow()
      read_shared(imp$file)
    }, error = identity)
    if (inherits(value, "error")) {
      return(list(error = sprintf("Error: cannot import '%s' from %s: %s",
                                  imp$name, imp$file,
                                  conditionMessage(value))))
    }
    values[imp$name] <- list(value)
  }
  list(values = values)
}

# Returns list(files = <list of {name, file}>) or list(error = <error output
# text>); on error, no file written by this call is left behind.
export_values <- function(names, env) {
  written <- list()
  for (name in names) {
    file <- next_shared_path()
    result <- tryCatch({
      need_nanoarrow()
      if (!exists(name, envir = env, inherits = FALSE)) {
        share_error("there is no variable '", name, "' in the session")
      }
      write_shared(get(name, envir = env, inherits = FALSE), file)
      NULL
    }, error = identity)
    if (!is.null(result)) {
      unlink(c(file, vapply(written, function(w) w$file, "")))
      return(list(error = sprintf("Error: cannot export '%s': %s", name,
                                  conditionMessage(result))))
    }
    written[[length(written) + 1L]] <- list(name = name, file = file)
  }
  list(files = written)
}

# ---------------------------------------------------------------------------
# Requests

protocol_error <- function(msg) {
  stop(structure(class = c("loom_protocol_error", "error", "condition"),
                 list(message = msg, call = NULL)))
}

is_string <- function(v) is.character(v) && length(v) == 1L && !is.na(v)

field_string <- function(req, name) {
  v <- req[[name]]
  if (is.null(v)) protocol_error(sprintf("missing field '%s'", name))
  if (!is_string(v)) protocol_error(sprintf("field '%s' must be a string", name))
  v
}

field_position <- function(req, name) {
  v <- req[[name]]
  if (!is.null(v) && !is_string(v)) {
    protocol_error(sprintf("field '%s' must be a string or null", name))
  }
  v
}

field_setup <- function(req) {
  setup <- req[["setup"]]
  if (is.null(setup)) return(character())
  if (!is.list(setup) || !all(vapply(setup, is_string, logical(1)))) {
    protocol_error("field 'setup' must be an array of strings")
  }
  vapply(setup, identity, "")
}

field_where <- function(req) {
  file <- req[["file"]]
  line <- req[["line"]]
  if (!is.null(file) && !is_string(file)) {
    protocol_error("field 'file' must be a string")
  }
  if (!is.null(line)) {
    if (!is.numeric(line) || length(line) != 1L || !is.finite(line) ||
        line < 1 || line != round(line)) {
      protocol_error("field 'line' must be a positive integer")
    }
    line <- as.integer(line)
  }
  list(file = file, line = line)
}

field_options <- function(req) {
  o <- req[["options"]]
  if (!is.list(o) || is.null(names(o))) {
    protocol_error("field 'options' must be an object")
  }
  number <- function(key) {
    v <- o[[key]]
    if (!is.numeric(v) || length(v) != 1L || !is.finite(v) || v <= 0) {
      protocol_error(sprintf("option '%s' must be a positive number", key))
    }
    as.numeric(v)
  }
  fmt <- o[["fig-format"]]
  if (!is_string(fmt) || !fmt %in% names(mime_types)) {
    protocol_error("option 'fig-format' must be one of svg, png, pdf")
  }
  pw <- o[["print-width"]]
  if (!is.numeric(pw) || length(pw) != 1L || !is.finite(pw) || pw < 20 ||
      pw != round(pw)) {
    protocol_error("option 'print-width' must be an integer of at least 20")
  }
  list(width = number("fig-width"), height = number("fig-height"),
       format = fmt, dpi = number("fig-dpi"), print_width = as.integer(pw))
}

field_imports <- function(req) {
  imports <- req[["imports"]]
  if (is.null(imports)) return(list())
  valid <- is.list(imports) && all(vapply(imports, function(i) {
    is.list(i) && is_string(i[["name"]]) && nzchar(i[["name"]]) &&
      is_string(i[["file"]])
  }, logical(1)))
  if (!valid) {
    protocol_error("field 'imports' must be an array of {\"name\", \"file\"} objects")
  }
  lapply(imports, function(i) list(name = i[["name"]], file = i[["file"]]))
}

# NULL when the request has no `exports`, which then also has no `exports`
# in its response.
field_exports <- function(req) {
  exports <- req[["exports"]]
  if (is.null(exports)) return(NULL)
  if (!is.list(exports) || !all(vapply(exports, is_string, logical(1)))) {
    protocol_error("field 'exports' must be an array of strings")
  }
  vapply(exports, identity, "")
}

show_position <- function(p) if (is.null(p)) "null" else p

op_hello <- function() {
  list(ok = TRUE, protocol = 1L, language = "r",
       version = as.character(getRversion()), pid = Sys.getpid())
}

op_reset <- function(req) {
  name <- field_string(req, "session")
  position <- field_position(req, "position")
  setup <- field_setup(req)
  rec <- new.env(parent = emptyenv())
  rec$env <- new.env(parent = globalenv())
  rec$position <- NULL
  assign(name, rec, envir = state$sessions)
  err <- run_setup(rec$env, setup)
  if (!is.null(err)) {
    return(list(ok = TRUE, failed = TRUE,
                outputs = list(list(type = "error", text = err))))
  }
  rec$position <- position
  list(ok = TRUE, failed = FALSE, outputs = list())
}

op_exec <- function(req) {
  name <- field_string(req, "session")
  after <- field_position(req, "after")
  id <- field_string(req, "id")
  mode <- field_string(req, "mode")
  code <- field_string(req, "code")
  opts <- field_options(req)
  # Characters per line of printed output (the `print-width` option).
  options(width = opts$print_width)
  where <- field_where(req)
  imports <- field_imports(req)
  exports <- field_exports(req)
  if (!mode %in% c("block", "inline")) {
    protocol_error(sprintf("field 'mode' must be 'block' or 'inline', not '%s'",
                           mode))
  }
  rec <- get0(name, envir = state$sessions, inherits = FALSE)
  if (is.null(rec)) {
    protocol_error(sprintf(
      "position mismatch: session '%s' does not exist, request expects %s",
      name, show_position(after)))
  }
  if (!identical(rec$position, after)) {
    protocol_error(sprintf(
      "position mismatch: session '%s' is at %s, request expects %s",
      name, show_position(rec$position), show_position(after)))
  }
  # Stays null if the kernel fails internally, so that loom rebuilds the
  # session instead of trusting a state it cannot know.
  before <- binding_snapshot(rec$env)
  rec$position <- NULL
  # All imports are read before any is bound, so a failed import binds
  # nothing; the code then does not run.
  loaded <- if (length(imports)) import_values(imports)
            else list(values = list())
  if (!is.null(loaded$error)) {
    outputs <- list(list(type = "error", text = loaded$error))
    if (mode == "block" && nzchar(code)) {
      outputs <- c(list(list(type = "source", code = code)), outputs)
    }
    result <- list(failed = TRUE, outputs = outputs)
  } else {
    for (imported in names(loaded$values)) {
      assign(imported, loaded$values[[imported]], envir = rec$env)
    }
    result <- if (mode == "block") exec_block(code, rec$env, opts, where)
              else exec_inline(code, rec$env, where)
  }
  exported <- list()
  if (length(exports) && !result$failed) {
    written <- export_values(exports, rec$env)
    if (is.null(written$error)) {
      exported <- written$files
    } else {
      result$failed <- TRUE
      result$outputs <- c(result$outputs,
                          list(list(type = "error", text = written$error)))
    }
  }
  rec$position <- id
  # Imported names are reported even when the value comparison in
  # changed_names() finds them equal to what they replaced.
  changed <- sort(unique(c(changed_names(before, binding_snapshot(rec$env)),
                           names(loaded$values))), method = "radix")
  # Lists keep length-1 vectors as JSON arrays under auto_unbox.
  response <- list(ok = TRUE, failed = result$failed, outputs = result$outputs,
                   changed = as.list(changed), packages = loaded_packages())
  if (!is.null(exports)) response$exports <- exported
  response
}

run_request <- function(req) {
  op <- req[["op"]]
  if (!is_string(op)) protocol_error("missing or invalid field 'op'")
  switch(op,
    hello = op_hello(),
    reset = op_reset(req),
    exec = op_exec(req),
    shutdown = list(ok = TRUE),
    protocol_error(sprintf("unknown op '%s'", op)))
}

decode_request <- function(bytes) {
  n <- length(bytes)
  if (n > 0L && bytes[[n]] == as.raw(13L)) bytes <- bytes[-n]
  if (any(bytes == as.raw(0L))) stop("request contains a NUL byte")
  text <- rawToChar(bytes)
  Encoding(text) <- "UTF-8"
  if (!validUTF8(text)) stop("request is not valid UTF-8")
  req <- jsonlite::parse_json(text, simplifyVector = FALSE)
  if (!is.list(req) || is.null(names(req))) {
    stop("request is not a JSON object")
  }
  req
}

# Returns list(response, exit, status); a non-NULL `exit` is the log message
# with which the kernel exits, with `status`, after sending the response.
handle_line <- function(bytes) {
  req <- tryCatch(decode_request(bytes), error = identity)
  if (inherits(req, "error")) {
    return(list(response = list(ok = FALSE, error = paste0(
      "malformed request: ", conditionMessage(req)))))
  }
  if (!identical(req[["token"]], state$token)) {
    return(list(response = list(ok = FALSE, error = "invalid token"),
                exit = "request with an invalid token; exiting", status = 1L))
  }
  calls <- NULL
  response <- tryCatch(
    withCallingHandlers(run_request(req), error = function(e) {
      if (!inherits(e, "loom_protocol_error")) calls <<- sys.calls()
    }),
    loom_protocol_error = function(e) list(ok = FALSE, error = conditionMessage(e)),
    interrupt = function(e) list(ok = FALSE, error = "interrupted"),
    error = function(e) {
      kernel_log("internal error: ", conditionMessage(e), "\n",
                 paste0("  ", vapply(calls, deparse_call, ""), collapse = "\n"))
      list(ok = FALSE, error = paste0("internal error: ", conditionMessage(e)))
    })
  shutdown <- identical(req[["op"]], "shutdown") && isTRUE(response$ok)
  list(response = response,
       exit = if (shutdown) "shutting down on request", status = 0L)
}

encode_response <- function(response) {
  response <- rapply(response, utf8, classes = "character", how = "replace")
  json <- jsonlite::toJSON(response, auto_unbox = TRUE, null = "null",
                           na = "null", digits = NA)
  c(charToRaw(enc2utf8(as.character(json))), as.raw(10L))
}

# A socket write first waits in select() until the socket is writable, and
# R delivers a SIGINT there even while interrupts are suspended, abandoning
# the write. select() reports a TCP socket writable only when at least a few
# kilobytes of buffer are free, so a chunk of at most `chunk` bytes is either
# written whole or not at all; an interrupted chunk is written again.
send <- function(con, response, chunk = 1024L) {
  bytes <- encode_response(response)
  n <- length(bytes)
  offset <- 0L
  while (offset < n) {
    piece <- bytes[(offset + 1L):min(offset + chunk, n)]
    result <- tryCatch({
      writeBin(piece, con)
      NULL
    }, interrupt = identity, warning = identity, error = identity)
    if (is.null(result)) {
      offset <- offset + length(piece)
    } else if (inherits(result, "interrupt")) {
      kernel_log("interrupt received while writing a response; ignored")
    } else {
      kernel_log("could not send a response: ", conditionMessage(result))
      return(FALSE)
    }
  }
  TRUE
}

# ---------------------------------------------------------------------------
# Transport

stop_kernel <- function(status, ...) {
  kernel_log(...)
  quit(save = "no", status = status)
}

exit_if_orphaned <- function() {
  if (!isTRUE(tools::pskill(state$parent_pid, 0L))) {
    stop_kernel(0L, "parent process ", state$parent_pid, " is gone; exiting")
  }
}

# Waits up to two seconds for `con` to become readable. socketSelect() waits
# in select(), which delivers a SIGINT even while interrupts are suspended;
# arriving while no request runs, it is ignored.
wait_readable <- function(con) {
  tryCatch(socketSelect(list(con), write = FALSE, timeout = 2),
           interrupt = function(cnd) {
             kernel_log("interrupt received while idle; ignored")
             FALSE
           })
}

# Delivers a SIGINT that arrived while interrupts were suspended, and
# ignores it.
discard_interrupt <- function() {
  tryCatch(allowInterrupts(Sys.sleep(0.001)), interrupt = function(cnd) {
    kernel_log("interrupt received while no request was running; ignored")
  })
}

# Sends the token line, then answers requests until loom closes the
# connection. Reads are non-blocking and waiting happens in socketSelect(),
# so an idle connection never times out; the connection's timeout only
# bounds writes.
#
# Interrupts are suspended in the kernel's own code and allowed only while
# user code runs (allowInterrupts() in the exec and setup paths), so a SIGINT
# interrupts a running request and is otherwise ignored, and never cuts a
# request or a response in half.
serve <- function(con) suspendInterrupts({
  if (!send(con, list(token = state$token))) {
    stop_kernel(1L, "could not send the token line; exiting")
  }
  buffer <- raw()
  eof <- FALSE
  repeat {
    exit_if_orphaned()
    newline <- match(as.raw(10L), buffer)
    if (is.na(newline)) {
      if (eof) {
        if (length(buffer)) {
          kernel_log("loom closed the connection inside a request; ",
                     length(buffer), " bytes discarded")
        }
        stop_kernel(0L, "loom closed the connection; exiting")
      }
      if (isTRUE(wait_readable(con))) {
        bytes <- tryCatch(read_available(con), error = identity)
        if (inherits(bytes, "error")) {
          stop_kernel(1L, "read failed: ", conditionMessage(bytes), "; exiting")
        }
        # A short read without data pending means loom closed the
        # connection.
        eof <- !isIncomplete(con)
        buffer <- c(buffer, bytes)
      }
      next
    }
    line <- buffer[seq_len(newline - 1L)]
    buffer <- buffer[-seq_len(newline)]
    discard_interrupt()
    action <- handle_line(line)
    discard_interrupt()
    if (!send(con, action$response)) {
      stop_kernel(1L, "the connection is broken; exiting")
    }
    if (!is.null(action$exit)) {
      close(con)
      stop_kernel(action$status, action$exit)
    }
  }
})

parse_args <- function(args) {
  known <- c("--connect", "--token", "--parent-pid", "--scratch")
  values <- list()
  i <- 1L
  while (i <= length(args)) {
    flag <- args[[i]]
    if (!flag %in% known) stop("unknown argument '", flag, "'", call. = FALSE)
    if (i == length(args)) stop("argument ", flag, " needs a value", call. = FALSE)
    values[[flag]] <- args[[i + 1L]]
    i <- i + 2L
  }
  missing <- setdiff(known, names(values))
  if (length(missing)) {
    stop("missing arguments: ", paste(missing, collapse = ", "), call. = FALSE)
  }
  port <- suppressWarnings(as.integer(values[["--connect"]]))
  if (is.na(port) || port < 1L || port > 65535L) {
    stop("invalid --connect '", values[["--connect"]], "'", call. = FALSE)
  }
  pid <- suppressWarnings(as.integer(values[["--parent-pid"]]))
  if (is.na(pid) || pid < 1L) {
    stop("invalid --parent-pid '", values[["--parent-pid"]], "'", call. = FALSE)
  }
  scratch <- values[["--scratch"]]
  dir.create(scratch, recursive = TRUE, showWarnings = FALSE)
  if (!dir.exists(scratch)) {
    stop("cannot create the scratch directory '", scratch, "'", call. = FALSE)
  }
  list(port = port, token = values[["--token"]], parent_pid = pid,
       scratch = normalizePath(scratch, mustWork = TRUE))
}

main <- function() {
  args <- parse_args(commandArgs(trailingOnly = TRUE))
  state$token <- args$token
  state$parent_pid <- args$parent_pid
  state$scratch <- args$scratch
  # Warnings outside evaluate() (setup strings, inline code) go to the log
  # immediately; a deferred warning would never be printed, since the kernel
  # never returns to R's top level.
  # Plots made outside a cell's recording device must not create Rplots.pdf
  # in the document directory.
  options(warn = 1L, device = null_device)
  exit_if_orphaned()
  con <- tryCatch(
    socketConnection(host = "127.0.0.1", port = args$port, server = FALSE,
                     blocking = FALSE, open = "r+b", timeout = 86400L),
    error = identity)
  if (inherits(con, "error")) {
    stop_kernel(1L, "cannot connect to 127.0.0.1:", args$port, ": ",
                conditionMessage(con))
  }
  kernel_log("connected to 127.0.0.1:", args$port, " (pid ", Sys.getpid(), ")")
  serve(con)
}

main()

}, envir = new.env(parent = baseenv()))
