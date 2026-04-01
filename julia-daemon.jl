"""
julia-daemon.jl  —  Persistent Julia server with named sessions.

Usage:
    julia --startup-file=no --project=. julia-daemon.jl --port 2159

Each chapter in the book gets its own Julia Module (a "session").  The daemon
keeps a Dict mapping session names to Module objects.  Sessions are created
lazily on first use and can be reset on demand.

Protocol: newline-delimited JSON over TCP.

Request:
  { "session":       "intro",
    "id":            "cell-id",
    "code":          "julia code to run",
    "op":            "run" | "console" | "plot" | "reset" | "ping",
    "preamble_code": "code to replay after reset (optional)" }

Response:
  { "session":    "intro",
    "id":          "cell-id",
    "stdout":      "captured output",
    "stderr":      "captured warnings",
    "figures":     ["_loom_cache/intro/figures/cell-id-0.svg"],
    "error":       null | "exception message",
    "statements":  [{"code": "x=1", "output": "1"}, ...] }

Session lifecycle
-----------------
  run     — execute code in the named session module (create if absent)
  console — execute code per-statement, returning interleaved code/output
  reset   — discard the module, create a fresh one, then run preamble_code
  ping    — no-op health check; returns empty response
"""

using Sockets
using JSON3

# Force GR to use a non-interactive (null) workstation type.
# This prevents "print device already activated" errors when
# Plots.jl/GR figures are saved repeatedly across session resets.
ENV["GKSwstype"] = "nul"

# ── CLI ──────────────────────────────────────────────────────────────────────

function parse_port()
    for i in 1:length(ARGS)-1
        ARGS[i] == "--port" && return parse(Int, ARGS[i+1])
    end
    return 2159
end

function parse_idle_timeout()
    for i in 1:length(ARGS)-1
        ARGS[i] == "--idle-timeout" && return parse(Int, ARGS[i+1])
    end
    return 0  # 0 = no timeout
end

# Global timestamp of last activity, updated on every request.
const _last_activity = Ref(time())

function touch_activity!()
    _last_activity[] = time()
end

# ── Session Module management ────────────────────────────────────────────────

"""
Create a fresh bare module with the jlpp figure-saving helpers injected.
"""
function new_session_module(name::Symbol = :JlppSession)
    m = Module(name, true)          # bare module inheriting from Core
    Core.eval(m, quote
        # Per-cell figure accumulator — reset before each cell.
        const _jlpp_figures = String[]

        """
            jlpp_savefig(fig, name; fmt=:svg)

        Save `fig` (a Plots.jl, Makie, or compatible figure) to
        `_loom_cache/<session>/figures/<name>.<fmt>` and record the path so
        jlpp can embed it in the Typst document.
        """
        function jlpp_savefig(fig, name::AbstractString;
                              fmt::Symbol = :svg,
                              width::Real = parse(Float64, get(ENV, "LOOM_FIG_WIDTH", "7")),
                              height::Real = parse(Float64, get(ENV, "LOOM_FIG_HEIGHT", "5")))
            dir = get(ENV, "LOOM_FIG_DIR", joinpath("_loom_cache", "figures"))
            mkpath(dir)
            path = joinpath(dir, "$(name).$(fmt)")
            @info "jlpp_savefig: saving '$name' to $path ($(width)×$(height) in, fig type=$(typeof(fig)))"
            @info "  GKSwstype=$(get(ENV, "GKSwstype", "<unset>"))"
            try
                # Convert inches to pixels for Plots.jl (100 DPI)
                wpx = round(Int, width * 100)
                hpx = round(Int, height * 100)
                _save_figure(fig, path, wpx, hpx)
                fsize = isfile(path) ? filesize(path) : -1
                @info "  saved OK, file size = $fsize bytes"
                if fsize == 0
                    error("Figure save produced a 0-byte file: $path")
                end
                push!(_jlpp_figures, path)
            catch e
                @warn "jlpp_savefig failed for $name" exception = (e, catch_backtrace())
            end
            return nothing
        end

        function _reset_plots_backend(mod)
            if !isdefined(mod, :Plots)
                return false
            end

            plots_mod = getfield(mod, :Plots)
            if !isdefined(plots_mod, :GR)
                return false
            end

            gr_mod = getfield(plots_mod, :GR)
            reset_any = false

            if isdefined(gr_mod, :emergencyclosegks)
                try
                    @info "  resetting GR via emergencyclosegks()"
                    Base.invokelatest(getfield(gr_mod, :emergencyclosegks))
                    reset_any = true
                catch e
                    @warn "  GR emergencyclosegks() failed" exception = (e, catch_backtrace())
                end
            end

            if isdefined(gr_mod, :reset)
                try
                    @info "  resetting GR via reset()"
                    Base.invokelatest(getfield(gr_mod, :reset))
                    reset_any = true
                catch e
                    @warn "  GR reset() failed" exception = (e, catch_backtrace())
                end
            end

            return reset_any
        end

        function _save_figure(fig, path::AbstractString, wpx::Int, hpx::Int)
            mod = @__MODULE__
            @info "  _save_figure: savefig=$(isdefined(mod, :savefig)), save=$(isdefined(mod, :save))"
            for attempt in 1:2
                if attempt == 2
                    reset_ok = _reset_plots_backend(mod)
                    reset_ok || error("Figure save failed and no Plots.GR reset hook was available")
                    rm(path; force=true)
                end

                if isdefined(mod, :savefig)
                    @info "  calling savefig(fig, $path) [attempt $attempt]"
                    Base.invokelatest(getfield(mod, :savefig), fig, path)
                    @info "  savefig returned"
                elseif isdefined(mod, :save)
                    @info "  calling save($path, fig; size=($wpx, $hpx)) [attempt $attempt]"
                    Base.invokelatest(getfield(mod, :save), path, fig;
                                      size = (wpx, hpx))
                    @info "  save returned"
                else
                    error("No savefig or save function available — load Plots or Makie first")
                end

                fsize = isfile(path) ? filesize(path) : -1
                if fsize > 0
                    return nothing
                end

                @warn "  figure file is empty after attempt $attempt" path fsize
            end

            error("Figure save produced a 0-byte file after retry: $path")
        end
    end)
    return m
end

# ── Cell execution ───────────────────────────────────────────────────────────

"""
Execute `code` inside `mod`, with ENV["LOOM_FIG_DIR"] pointing to
`fig_dir` so jlpp_savefig saves figures in the right chapter subdirectory.

Returns (stdout, stderr, figures, error_or_nothing).
"""
function run_cell(mod::Module, code::String, cell_id::String, fig_dir::String)
    @info "run_cell: id=$cell_id, module=$(nameof(mod)), fig_dir=$fig_dir"
    # Reset the figure accumulator.
    try Core.eval(mod, :(empty!(_jlpp_figures))) catch e;
        @warn "  failed to clear _jlpp_figures" exception = e
    end

    old_figdir = get(ENV, "LOOM_FIG_DIR", "")
    ENV["LOOM_FIG_DIR"] = fig_dir
    mkpath(fig_dir)

    result_error = nothing
    stdout_str = ""
    stderr_str = ""

    # Use temp files for stdout/stderr capture (IOBuffer not supported by
    # redirect_stdout in Julia 1.12+).
    stdout_file = tempname()
    stderr_file = tempname()

    try
        open(stdout_file, "w") do out_f
            open(stderr_file, "w") do err_f
                redirect_stdout(out_f) do
                    redirect_stderr(err_f) do
                        include_string(mod, code, "cell:$(cell_id)")
                    end
                end
            end
        end
    catch e
        result_error = sprint(showerror, e, catch_backtrace())
    finally
        ENV["LOOM_FIG_DIR"] = old_figdir
    end

    stdout_str = isfile(stdout_file) ? read(stdout_file, String) : ""
    stderr_str = isfile(stderr_file) ? read(stderr_file, String) : ""
    rm(stdout_file, force=true)
    rm(stderr_file, force=true)

    figures = String[]
    try figures = copy(Core.eval(mod, :(_jlpp_figures))) catch; end

    return stdout_str, stderr_str, figures, result_error
end

# ── Console-mode execution (per-statement, interleaved output) ──────────────

"""
Execute `code` statement-by-statement inside `mod`, capturing each
statement's printed output and REPL display value.  Returns a vector of
`Dict(:code => "...", :output => "...")` plus aggregate stderr/figures/error.
"""
function run_console_cell(mod::Module, code::String, cell_id::String, fig_dir::String)
    try Core.eval(mod, :(empty!(_jlpp_figures))) catch; end

    old_figdir = get(ENV, "LOOM_FIG_DIR", "")
    ENV["LOOM_FIG_DIR"] = fig_dir
    mkpath(fig_dir)

    # Capture stderr for the whole block.
    stderr_file = tempname()
    stderr_f = open(stderr_file, "w")
    old_stderr = stderr
    redirect_stderr(stderr_f)

    statements = Dict{Symbol,Any}[]
    global_error = nothing
    pos = 1
    code_len = ncodeunits(code)

    try
        while pos <= code_len
            # Parse the next expression.
            expr, next_pos = Meta.parse(code, pos; greedy=true, raise=false)

            # Extract source text for this expression.
            src = strip(String(code[pos:prevind(code, next_pos)]))
            pos = next_pos

            # Skip empty chunks (trailing whitespace, blank lines).
            isempty(src) && continue

            # Check for trailing semicolon (suppresses display).
            suppress = endswith(rstrip(src), ';')

            # nothing from parser means a pure comment or blank.
            if expr === nothing
                push!(statements, Dict(:code => src, :output => ""))
                continue
            end

            # Handle parse errors.
            if Meta.isexpr(expr, :error) || Meta.isexpr(expr, :incomplete)
                push!(statements, Dict(:code => src, :output => "ERROR: syntax error"))
                continue
            end

            # Execute and capture stdout + display.
            stdout_file = tempname()
            local result
            local result_error = nothing
            try
                result = open(stdout_file, "w") do f
                    redirect_stdout(f) do
                        Base.invokelatest(Core.eval, mod, expr)
                    end
                end
            catch e
                result_error = sprint(showerror, e)
            end
            captured_stdout = isfile(stdout_file) ? read(stdout_file, String) : ""
            rm(stdout_file, force=true)

            # Build the output string.
            output = ""
            if result_error !== nothing
                output = "ERROR: " * result_error
            else
                if !isempty(captured_stdout)
                    output = captured_stdout
                end
                if !suppress && result !== nothing
                    display_str = try
                        sprint(show, MIME("text/plain"), result)
                    catch
                        sprint(show, result)
                    end
                    if !isempty(output) && !endswith(output, '\n')
                        output *= "\n"
                    end
                    output *= display_str
                end
            end

            push!(statements, Dict(:code => src, :output => output))
        end
    catch e
        global_error = sprint(showerror, e, catch_backtrace())
    finally
        redirect_stderr(old_stderr)
        close(stderr_f)
        ENV["LOOM_FIG_DIR"] = old_figdir
    end

    stderr_str = isfile(stderr_file) ? read(stderr_file, String) : ""
    rm(stderr_file, force=true)

    figures = String[]
    try figures = copy(Core.eval(mod, :(_jlpp_figures))) catch; end

    # Also build aggregate stdout for backward compat.
    all_stdout = join([s[:output] for s in statements if !isempty(s[:output])], "\n")

    return all_stdout, stderr_str, figures, global_error, statements
end

# ── Table-mode execution (captures MIME"text/typst" output) ──────────────────

"""
Execute `code` inside `mod`, then capture `show(io, MIME"text/typst", result)`
on the last expression's return value.  Returns the standard run_cell tuple
plus a `typst_output` string containing raw Typst markup.
"""
function run_table_cell(mod::Module, code::String, cell_id::String, fig_dir::String)
    try Core.eval(mod, :(empty!(_jlpp_figures))) catch; end

    old_figdir = get(ENV, "LOOM_FIG_DIR", "")
    ENV["LOOM_FIG_DIR"] = fig_dir
    mkpath(fig_dir)

    result_error = nothing
    stdout_str = ""
    stderr_str = ""
    typst_output = ""
    result = nothing

    # Execute the full block, capturing the return value of the last expression.
    stdout_file = tempname()
    stderr_file = tempname()
    try
        result = open(stdout_file, "w") do out_f
            open(stderr_file, "w") do err_f
                redirect_stdout(out_f) do
                    redirect_stderr(err_f) do
                        Base.invokelatest(include_string, mod, code, "cell:$(cell_id)")
                    end
                end
            end
        end
    catch e
        result_error = sprint(showerror, e, catch_backtrace())
    finally
        ENV["LOOM_FIG_DIR"] = old_figdir
    end

    stdout_str = isfile(stdout_file) ? read(stdout_file, String) : ""
    stderr_str = isfile(stderr_file) ? read(stderr_file, String) : ""
    rm(stdout_file, force=true)
    rm(stderr_file, force=true)

    figures = String[]
    try figures = copy(Core.eval(mod, :(_jlpp_figures))) catch; end

    # Capture MIME"text/typst" representation of the result.
    if result_error === nothing && result !== nothing
        try
            typst_output = Base.invokelatest(sprint, show, MIME("text/typst"), result)
        catch e
            # Not all types support text/typst — that's OK, fall back to empty.
            @warn "text/typst output not available for $(typeof(result))" exception=e
        end
    end

    return stdout_str, stderr_str, figures, result_error, typst_output
end

"""
    run_expr_cell(mod, code, cell_id, fig_dir)

Execute `code` and capture the last expression's value as a Typst string.
Unlike `run_table_cell` (which uses MIME dispatch), this treats the result
as a plain string — suitable for code that returns Typst math expressions
or other markup directly.
"""
function run_expr_cell(mod::Module, code::String, cell_id::String, fig_dir::String)
    try Core.eval(mod, :(empty!(_jlpp_figures))) catch; end

    old_figdir = get(ENV, "LOOM_FIG_DIR", "")
    ENV["LOOM_FIG_DIR"] = fig_dir
    mkpath(fig_dir)

    result_error = nothing
    stdout_str = ""
    stderr_str = ""
    typst_output = ""
    result = nothing

    stdout_file = tempname()
    stderr_file = tempname()
    try
        result = open(stdout_file, "w") do out_f
            open(stderr_file, "w") do err_f
                redirect_stdout(out_f) do
                    redirect_stderr(err_f) do
                        Base.invokelatest(include_string, mod, code, "cell:$(cell_id)")
                    end
                end
            end
        end
    catch e
        result_error = sprint(showerror, e, catch_backtrace())
    finally
        ENV["LOOM_FIG_DIR"] = old_figdir
    end

    stdout_str = isfile(stdout_file) ? read(stdout_file, String) : ""
    stderr_str = isfile(stderr_file) ? read(stderr_file, String) : ""
    rm(stdout_file, force=true)
    rm(stderr_file, force=true)

    figures = String[]
    try figures = copy(Core.eval(mod, :(_jlpp_figures))) catch; end

    if result_error === nothing && result !== nothing
        typst_output = result isa AbstractString ? result : string(result)
    end

    return stdout_str, stderr_str, figures, result_error, typst_output
end

function run_plot_cell(mod::Module, code::String, cell_id::String, fig_dir::String)
    try Core.eval(mod, :(empty!(_jlpp_figures))) catch; end

    old_figdir = get(ENV, "LOOM_FIG_DIR", "")
    ENV["LOOM_FIG_DIR"] = fig_dir
    mkpath(fig_dir)

    result_error = nothing
    stdout_str = ""
    stderr_str = ""
    result = nothing

    stdout_file = tempname()
    stderr_file = tempname()
    try
        result = open(stdout_file, "w") do out_f
            open(stderr_file, "w") do err_f
                redirect_stdout(out_f) do
                    redirect_stderr(err_f) do
                        Base.invokelatest(include_string, mod, code, "cell:$(cell_id)")
                    end
                end
            end
        end
    catch e
        result_error = sprint(showerror, e, catch_backtrace())
    finally
        ENV["LOOM_FIG_DIR"] = old_figdir
    end

    stdout_str = isfile(stdout_file) ? read(stdout_file, String) : ""
    stderr_str = isfile(stderr_file) ? read(stderr_file, String) : ""
    rm(stdout_file, force=true)
    rm(stderr_file, force=true)

    figures = String[]
    try figures = copy(Core.eval(mod, :(_jlpp_figures))) catch; end

    if result_error === nothing && isempty(figures) && result !== nothing
        try
            Base.invokelatest(getfield(mod, :jlpp_savefig), result, cell_id; fmt=:svg)
        catch e
            result_error = sprint(showerror, e, catch_backtrace())
        end
        try figures = copy(Core.eval(mod, :(_jlpp_figures))) catch; end
    end

    if result_error === nothing && isempty(figures)
        result_error = "No figure was captured from the cell result. Return a plot object or use jlrun + jlpp_savefig."
    end

    return stdout_str, stderr_str, figures, result_error
end

# ── Request / response ───────────────────────────────────────────────────────

struct Request
    session::String
    id::String
    code::String
    op::String              # "run" | "plot" | "reset" | "ping"
    preamble_code::String
end

function parse_request(line::AbstractString)
    obj = JSON3.read(line)
    req = Request(
        get(obj, :session,       "__default__"),
        get(obj, :id,            ""),
        get(obj, :code,          ""),
        get(obj, :op,            "run"),
        get(obj, :preamble_code, ""),
    )
    # Extract fig dimensions from options if present.
    opts = get(obj, :options, nothing)
    if opts !== nothing
        fw = get(opts, Symbol("fig-width"), get(opts, :fig_width, nothing))
        fh = get(opts, Symbol("fig-height"), get(opts, :fig_height, nothing))
        if fw !== nothing; ENV["LOOM_FIG_WIDTH"] = string(fw); end
        if fh !== nothing; ENV["LOOM_FIG_HEIGHT"] = string(fh); end
    else
        ENV["LOOM_FIG_WIDTH"] = "7"
        ENV["LOOM_FIG_HEIGHT"] = "5"
    end
    return req
end

function make_response(session, id, stdout, stderr, figures, error;
                       statements=Dict{Symbol,Any}[], typst_output="")
    Dict(
        :session      => session,
        :id           => id,
        :stdout       => stdout,
        :stderr        => stderr,
        :figures      => figures,
        :error        => error,
        :statements   => statements,
        :typst_output => typst_output,
    )
end

# ── Sessions store ───────────────────────────────────────────────────────────
#
# Keyed by session name (String).  All mutation goes through the helper
# functions below so we have a single point to add locking if needed.

const SESSIONS = Dict{String, Module}()

function get_or_create_session(name::String)::Module
    get!(SESSIONS, name) do
        @info "Creating session '$name'"
        new_session_module(Symbol("JlppSession_$(name)"))
    end
end

function reset_session!(name::String, preamble_code::String)
    @info "Resetting session '$name'"
    if haskey(SESSIONS, name)
        old_mod = SESSIONS[name]
        try
            Core.eval(old_mod, quote
                if isdefined(@__MODULE__, :_reset_plots_backend)
                    _reset_plots_backend(@__MODULE__)
                end
            end)
        catch e
            @warn "  Failed to reset plotting backend before discarding '$name'" exception = (e, catch_backtrace())
        end
    end
    delete!(SESSIONS, name)
    mod = get_or_create_session(name)
    if !isempty(preamble_code)
        @info "  Replaying preamble into '$name'…"
        fig_dir = joinpath("_loom_cache", name, "figures")
        out, err, _, err_msg = run_cell(mod, preamble_code, "__preamble__", fig_dir)
        isnothing(err_msg) || @warn "  Preamble replay error in '$name'" msg=err_msg
    end
    return mod
end

# ── Connection handler ───────────────────────────────────────────────────────

function handle_connection(conn::TCPSocket)
    try
        for line in eachline(conn)
            isempty(strip(line)) && continue
            touch_activity!()

            req = parse_request(line)

            if req.op == "ping"
                resp = make_response(req.session, req.id, "", "", String[], nothing)
                println(conn, JSON3.write(resp))
                continue
            end

            if req.op == "reset"
                reset_session!(req.session, req.preamble_code)
                resp = make_response(req.session, req.id, "", "", String[], nothing)
                println(conn, JSON3.write(resp))
                continue
            end

            mod     = get_or_create_session(req.session)
            fig_dir = joinpath("_loom_cache", req.session, "figures")

            if req.op == "console"
                stdout_str, stderr_str, figures, err, stmts =
                    run_console_cell(mod, req.code, req.id, fig_dir)
                resp = make_response(req.session, req.id, stdout_str, stderr_str,
                                     figures, err; statements=stmts)
            elseif req.op == "table"
                stdout_str, stderr_str, figures, err, typst_out =
                    run_table_cell(mod, req.code, req.id, fig_dir)
                resp = make_response(req.session, req.id, stdout_str, stderr_str,
                                     figures, err; typst_output=typst_out)
            elseif req.op == "expr"
                stdout_str, stderr_str, figures, err, typst_out =
                    run_expr_cell(mod, req.code, req.id, fig_dir)
                resp = make_response(req.session, req.id, stdout_str, stderr_str,
                                     figures, err; typst_output=typst_out)
            elseif req.op == "plot"
                stdout_str, stderr_str, figures, err =
                    run_plot_cell(mod, req.code, req.id, fig_dir)
                resp = make_response(req.session, req.id, stdout_str, stderr_str,
                                     figures, err)
            else  # op == "run"
                stdout_str, stderr_str, figures, err =
                    run_cell(mod, req.code, req.id, fig_dir)
                resp = make_response(req.session, req.id, stdout_str, stderr_str,
                                     figures, err)
            end
            println(conn, JSON3.write(resp))
        end
    catch e
        (isa(e, EOFError) || isa(e, Base.IOError)) || @warn "handle_connection error" exception=e
    finally
        close(conn)
    end
end

# ── Main ─────────────────────────────────────────────────────────────────────

function main()
    port    = parse_port()
    timeout = parse_idle_timeout()
    server  = listen(IPv4(0), port)
    @info "Julia daemon ready" port=port pid=getpid() julia=VERSION idle_timeout_s=timeout

    # PID file so loom can detect stale daemons.
    write(joinpath(tempdir(), "loom-daemon-$(port).pid"), string(getpid()))

    # Idle-timeout watchdog: check every 30 s whether we've been idle too long.
    if timeout > 0
        @async begin
            while true
                sleep(30)
                idle = time() - _last_activity[]
                if idle >= timeout
                    @info "Idle timeout reached ($(round(idle; digits=0)) s), shutting down."
                    exit(0)
                end
            end
        end
    end

    # Serial accept loop — cells from the same chapter arrive sequentially
    # (one per connection) because the Rust side awaits each response.
    # @async keeps the loop non-blocking for the rare case of parallel chapters.
    while true
        conn = accept(server)
        @async handle_connection(conn)
    end
end

main()
