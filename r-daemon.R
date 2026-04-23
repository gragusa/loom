#!/usr/bin/env Rscript
#
# r-daemon.R — Persistent R HTTP server for loom (httpuv-based)
#
# Protocol: JSON over HTTP POST on /eval
#
# Request body (JSON):
#   { "session": "intro", "id": "cell-id", "code": "R code",
#     "op": "run|console|table|reset|ping",
#     "preamble_code": "", "options": { ... } }
#
# Response body (JSON):
#   { "session": "intro", "id": "cell-id", "stdout": "...",
#     "stderr": "...", "figures": [...], "error": null,
#     "statements": [...], "typst_output": "" }

suppressPackageStartupMessages({
  library(jsonlite)
  library(httpuv)
})

`%||%` <- function(a, b) if (is.null(a)) b else a

# ── Session management ──────────────────────────────────────────────────────

sessions <- new.env(parent = emptyenv())

get_or_create_session <- function(name) {
  if (!exists(name, envir = sessions, inherits = FALSE)) {
    env <- new.env(parent = globalenv())
    env$loom_savefig <- function(plot = NULL, name, fmt = "svg",
                                 width = as.numeric(Sys.getenv("LOOM_FIG_WIDTH", "7")),
                                 height = as.numeric(Sys.getenv("LOOM_FIG_HEIGHT", "5"))) {
      dir <- Sys.getenv("LOOM_FIG_DIR", "_loom_cache/figures")
      dir.create(dir, recursive = TRUE, showWarnings = FALSE)
      path <- file.path(dir, paste0(name, ".", fmt))
      if (fmt == "svg") {
        svg(path, width = width, height = height)
      } else if (fmt == "pdf") {
        pdf(path, width = width, height = height)
      } else {
        png(path, width = width, height = height, units = "in", res = 150)
      }
      if (!is.null(plot)) {
        print(plot)
      } else {
        tryCatch({
          if (requireNamespace("ggplot2", quietly = TRUE)) {
            p <- ggplot2::last_plot()
            if (!is.null(p)) print(p)
          }
        }, error = function(e) NULL)
      }
      dev.off()
      env$.loom_figures <- c(env$.loom_figures, path)
      invisible(path)
    }
    env$.loom_figures <- character(0)
    assign(name, env, envir = sessions)
  }
  get(name, envir = sessions, inherits = FALSE)
}

reset_session <- function(name, preamble_code = "") {
  if (exists(name, envir = sessions, inherits = FALSE)) {
    rm(list = name, envir = sessions)
  }
  env <- get_or_create_session(name)
  if (nzchar(preamble_code)) {
    tryCatch(
      eval(parse(text = preamble_code), envir = env),
      error = function(e) warning("Preamble error: ", conditionMessage(e))
    )
  }
  env
}

# ── Output capture ──────────────────────────────────────────────────────────

run_with_capture <- function(code_str, env, opts) {
  msg_lines <- character(0)
  warn_lines <- character(0)
  err_msg <- NULL

  env$.loom_figures <- character(0)
  env$.loom_last_result <- NULL
  fig_dir <- Sys.getenv("LOOM_FIG_DIR", "_loom_cache/figures")
  dir.create(fig_dir, recursive = TRUE, showWarnings = FALSE)

  # Use temp file for output capture (reliable in httpuv context).
  tmp_out <- tempfile()
  file_con <- file(tmp_out, open = "w")
  sink(file_con, type = "output")
  tryCatch(
    withCallingHandlers(
      {
        exprs <- parse(text = code_str)
        for (expr in exprs) {
          # Store result in session env to avoid <<- scope issues in httpuv.
          env$.loom_last_result <- eval(expr, envir = env)
        }
      },
      message = function(m) {
        if (isTRUE(opts$message)) msg_lines <<- c(msg_lines, conditionMessage(m))
        invokeRestart("muffleMessage")
      },
      warning = function(w) {
        if (isTRUE(opts$warning)) warn_lines <<- c(warn_lines, conditionMessage(w))
        invokeRestart("muffleWarning")
      }
    ),
    error = function(e) {
      err_msg <<- conditionMessage(e)
    }
  )
  sink(type = "output")
  close(file_con)
  stdout_str <- paste(readLines(tmp_out, warn = FALSE), collapse = "\n")
  file.remove(tmp_out)
  result <- env$.loom_last_result

  stderr_parts <- character(0)
  if (length(msg_lines) > 0) stderr_parts <- c(stderr_parts, msg_lines)
  if (length(warn_lines) > 0) stderr_parts <- c(stderr_parts, paste0("Warning: ", warn_lines))
  stderr_str <- paste(stderr_parts, collapse = "\n")

  list(
    result = result,
    stdout = stdout_str,
    stderr = stderr_str,
    figures = env$.loom_figures,
    error = err_msg
  )
}

# ── Console mode ────────────────────────────────────────────────────────────

run_console <- function(code_str, env, opts) {
  statements <- list()
  env$.loom_figures <- character(0)
  first_error <- NULL

  exprs <- tryCatch(parse(text = code_str, keep.source = TRUE), error = function(e) {
    first_error <<- conditionMessage(e)
    statements[[1]] <<- list(
      code = jsonlite::unbox(code_str),
      output = jsonlite::unbox(paste0("Error: ", conditionMessage(e)))
    )
    return(NULL)
  })

  if (is.null(exprs)) {
    return(list(statements = statements, stderr = "", figures = character(0),
                error = first_error))
  }

  src_refs <- attr(exprs, "srcref")

  for (i in seq_along(exprs)) {
    expr <- exprs[[i]]

    if (!is.null(src_refs) && i <= length(src_refs)) {
      src <- paste(as.character(src_refs[[i]]), collapse = "\n")
    } else {
      src <- paste(deparse(expr, width.cutoff = 500), collapse = "\n")
    }

    expr_error <- NULL

    expr_error <- NULL

    # Use a temp file for output capture (sink/textConnection unreliable in httpuv).
    tmp_out <- tempfile()
    file_con <- file(tmp_out, open = "w")
    sink(file_con, type = "output")
    tryCatch(
      withCallingHandlers(
        {
          res <- withVisible(eval(expr, envir = env))
          if (res$visible) print(res$value)
        },
        message = function(m) invokeRestart("muffleMessage"),
        warning = function(w) invokeRestart("muffleWarning")
      ),
      error = function(e) { expr_error <<- conditionMessage(e) }
    )
    sink(type = "output")
    close(file_con)
    captured <- paste(readLines(tmp_out, warn = FALSE), collapse = "\n")
    file.remove(tmp_out)

    output <- ""
    if (!is.null(expr_error)) {
      output <- paste0("Error: ", expr_error)
      if (is.null(first_error)) first_error <- expr_error
    } else if (nzchar(captured)) {
      output <- captured
    }

    statements[[length(statements) + 1]] <- list(
      code = jsonlite::unbox(src),
      output = jsonlite::unbox(output)
    )
  }

  list(statements = statements, stderr = "", figures = env$.loom_figures,
       error = first_error)
}

# ── Table mode ──────────────────────────────────────────────────────────────

run_table <- function(code_str, env, opts) {
  cap <- run_with_capture(code_str, env, opts)
  typst_output <- ""

  if (is.null(cap$error) && !is.null(cap$result)) {
    typst_output <- tryCatch({
      # Try gt::as_typst (gt >= 0.12)
      fn <- tryCatch(getFromNamespace("as_typst", "gt"), error = function(e) NULL)
      if (!is.null(fn) && inherits(cap$result, "gt_tbl")) {
        return(list(stdout = cap$stdout, stderr = cap$stderr,
                    figures = cap$figures, error = cap$error,
                    typst_output = fn(cap$result)))
      }
      # gt without as_typst: try as_latex wrapped in raw block
      if (inherits(cap$result, "gt_tbl")) {
        fn <- tryCatch(getFromNamespace("as_latex", "gt"), error = function(e) NULL)
        if (!is.null(fn)) {
          latex_str <- as.character(fn(cap$result))
          # Wrap LaTeX in a Typst raw block
          typst_str <- paste0("$\n", latex_str, "\n$")
          return(list(stdout = cap$stdout, stderr = cap$stderr,
                      figures = cap$figures, error = cap$error,
                      typst_output = ""))
        }
      }
      # Try tinytable::save_tt
      if (inherits(cap$result, "tinytable")) {
        fn <- tryCatch(getFromNamespace("save_tt", "tinytable"), error = function(e) NULL)
        if (!is.null(fn)) {
          tmp <- tempfile(fileext = ".typ")
          fn(cap$result, tmp)
          out <- paste(readLines(tmp, warn = FALSE), collapse = "\n")
          file.remove(tmp)
          # Escape bare < > in table cell content to prevent Typst label parsing.
          # Only escape < > that appear inside [...] cell content, not Typst commands.
          out <- gsub("\\[(<)", "[\\\\<", out)
          return(list(stdout = cap$stdout, stderr = cap$stderr,
                      figures = cap$figures, error = cap$error,
                      typst_output = out))
        }
      }
      # No Typst output available
      ""
    }, error = function(e) {
      ""
    })
  }

  if (is.list(typst_output)) return(typst_output)
  list(stdout = cap$stdout, stderr = cap$stderr,
       figures = cap$figures, error = cap$error, typst_output = typst_output)
}

run_plot <- function(code_str, env, cell_id, opts) {
  cap <- run_with_capture(code_str, env, opts)

  if (is.null(cap$error) && length(cap$figures) == 0) {
    tryCatch(
      env$loom_savefig(cap$result, cell_id, fmt = "svg"),
      error = function(e) {
        cap$error <<- conditionMessage(e)
      }
    )
    cap$figures <- env$.loom_figures
  }

  if (is.null(cap$error) && length(cap$figures) == 0) {
    cap$error <- "No figure was captured from the cell result. Return a plot object or use rrun + loom_savefig."
  }

  cap
}

run_expr <- function(code_str, env, opts) {
  cap <- run_with_capture(code_str, env, opts)
  typst_out <- ""
  if (is.null(cap$error) && !is.null(cap$result)) {
    if (is.character(cap$result)) {
      typst_out <- paste(cap$result, collapse = "\n")
    } else {
      typst_out <- paste(as.character(cap$result), collapse = "\n")
    }
  }
  list(stdout = cap$stdout, stderr = cap$stderr,
       figures = cap$figures, error = cap$error, typst_output = typst_out)
}

# ── Request handling ────────────────────────────────────────────────────────

make_response <- function(session, id, stdout = "", stderr = "",
                          figures = character(0), error = NULL,
                          statements = list(), typst_output = "") {
  list(
    session = jsonlite::unbox(session),
    id = jsonlite::unbox(id),
    stdout = jsonlite::unbox(stdout),
    stderr = jsonlite::unbox(stderr),
    figures = figures,
    error = if (is.null(error)) jsonlite::unbox(NA) else jsonlite::unbox(error),
    statements = statements,
    typst_output = jsonlite::unbox(typst_output)
  )
}

default_options <- function() {
  list(message = TRUE, warning = TRUE, error = TRUE,
       results = "markup", fig_width = 7, fig_height = 5, comment = "##")
}

handle_request <- function(req) {
  session_name <- req$session %||% "__default__"
  cell_id <- req$id %||% ""
  code <- req$code %||% ""
  op <- req$op %||% "run"
  preamble_code <- req$preamble_code %||% ""

  opts <- default_options()
  if (!is.null(req$options)) {
    for (k in names(req$options)) opts[[k]] <- req$options[[k]]
  }

  if (op == "ping") return(make_response(session_name, cell_id))

  if (op == "reset") {
    reset_session(session_name, preamble_code)
    return(make_response(session_name, cell_id))
  }

  env <- get_or_create_session(session_name)
  fig_dir <- file.path("_loom_cache", session_name, "figures")
  Sys.setenv(LOOM_FIG_DIR = fig_dir)
  # Handle both hyphen and underscore variants from JSON options.
  fw <- opts[["fig-width"]] %||% opts[["fig_width"]] %||% opts$fig_width
  fh <- opts[["fig-height"]] %||% opts[["fig_height"]] %||% opts$fig_height
  Sys.setenv(LOOM_FIG_WIDTH = as.character(fw))
  Sys.setenv(LOOM_FIG_HEIGHT = as.character(fh))

  if (op == "console") {
    res <- run_console(code, env, opts)
    return(make_response(session_name, cell_id, stderr = res$stderr,
                         figures = res$figures, error = res$error,
                         statements = res$statements))
  }

  if (op == "table") {
    res <- run_table(code, env, opts)
    return(make_response(session_name, cell_id, stdout = res$stdout,
                         stderr = res$stderr, figures = res$figures,
                         error = res$error, typst_output = res$typst_output))
  }

  if (op == "expr") {
    res <- run_expr(code, env, opts)
    return(make_response(session_name, cell_id, stdout = res$stdout,
                         stderr = res$stderr, figures = res$figures,
                         error = res$error, typst_output = res$typst_output))
  }

  if (op == "plot") {
    cap <- run_plot(code, env, cell_id, opts)
    return(make_response(session_name, cell_id, stdout = cap$stdout,
                         stderr = cap$stderr, figures = cap$figures, error = cap$error))
  }

  # op == "run"
  cap <- run_with_capture(code, env, opts)
  make_response(session_name, cell_id, stdout = cap$stdout,
                stderr = cap$stderr, figures = cap$figures, error = cap$error)
}

# ── Main: httpuv server ─────────────────────────────────────────────────────

main <- function() {
  args <- commandArgs(trailingOnly = TRUE)
  port <- 2160L
  idle_timeout <- 0L  # 0 = no timeout

  i <- 1L
  while (i <= length(args)) {
    if (args[i] == "--port" && i < length(args)) {
      port <- as.integer(args[i + 1])
      i <- i + 2L
    } else if (args[i] == "--idle-timeout" && i < length(args)) {
      idle_timeout <- as.integer(args[i + 1])
      i <- i + 2L
    } else {
      i <- i + 1L
    }
  }

  # Use TMPDIR (matches Rust's std::env::temp_dir()), not R's tempdir()
  # which returns a session-specific subdirectory.
  tmp <- Sys.getenv("TMPDIR", Sys.getenv("TMP", "/tmp"))
  pid_file <- file.path(tmp, sprintf("loom-daemon-%d.pid", port))
  writeLines(as.character(Sys.getpid()), pid_file)

  # Track last activity for idle timeout.
  last_activity <- proc.time()[["elapsed"]]

  app <- list(
    call = function(req) {
      # Update activity timestamp on every request.
      last_activity <<- proc.time()[["elapsed"]]

      # Read request body
      body_raw <- req$rook.input$read(-1)
      body <- rawToChar(body_raw)

      if (!nzchar(body)) {
        return(list(
          status = 200L,
          headers = list("Content-Type" = "application/json"),
          body = "{}"
        ))
      }

      parsed <- tryCatch(
        fromJSON(body, simplifyVector = FALSE),
        error = function(e) NULL
      )

      if (is.null(parsed)) {
        return(list(
          status = 400L,
          headers = list("Content-Type" = "application/json"),
          body = '{"error":"invalid JSON"}'
        ))
      }

      resp <- handle_request(parsed)
      resp_json <- toJSON(resp, auto_unbox = FALSE, null = "null",
                          na = "null", force = TRUE)

      list(
        status = 200L,
        headers = list("Content-Type" = "application/json"),
        body = resp_json
      )
    }
  )

  cat(sprintf("R daemon (httpuv) listening on port %d (PID %d, idle_timeout=%ds)\n",
              port, Sys.getpid(), idle_timeout), file = stderr())

  if (idle_timeout > 0L) {
    # Use service() loop with periodic idle checks instead of blocking runServer().
    server <- startServer("0.0.0.0", port, app)
    on.exit(stopServer(server), add = TRUE)
    repeat {
      httpuv::service(timeoutMs = 30000L)
      idle <- proc.time()[["elapsed"]] - last_activity
      if (idle >= idle_timeout) {
        message(sprintf("Idle timeout reached (%.0f s), shutting down.", idle))
        break
      }
    }
  } else {
    runServer("0.0.0.0", port, app)
  }
}

main()
