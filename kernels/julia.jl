# Loom kernel for Julia.
#
# Executes code cells on behalf of `loom`. The wire protocol is specified in
# kernels/PROTOCOL.md (newline-delimited JSON over one TCP connection that
# the kernel opens to loom on 127.0.0.1).
#
# The kernel depends only on Julia's standard library so that it runs in any
# project environment without adding packages to it.

module LoomKernel

using Sockets
using Logging

const JS = Base.JuliaSyntax
const PROTOCOL_VERSION = 1

# ── JSON ─────────────────────────────────────────────────────────────────────

module MiniJSON

export parse_json, write_json

struct Reader
    s::String
    pos::Base.RefValue{Int}
end

peekbyte(r::Reader) = r.pos[] <= ncodeunits(r.s) ? codeunit(r.s, r.pos[]) : 0x00

function skipws(r::Reader)
    while r.pos[] <= ncodeunits(r.s) && codeunit(r.s, r.pos[]) in (0x20, 0x09, 0x0a, 0x0d)
        r.pos[] += 1
    end
end

function expect(r::Reader, lit::String)
    n = ncodeunits(lit)
    r.pos[] + n - 1 <= ncodeunits(r.s) && view(codeunits(r.s), r.pos[]:r.pos[]+n-1) == codeunits(lit) ||
        error("invalid JSON at byte $(r.pos[])")
    r.pos[] += n
end

function parse_json(s::AbstractString)
    r = Reader(String(s), Ref(1))
    v = read_value(r)
    skipws(r)
    r.pos[] > ncodeunits(r.s) || error("trailing data in JSON at byte $(r.pos[])")
    return v
end

function read_value(r::Reader)
    skipws(r)
    b = peekbyte(r)
    if b == UInt8('{')
        r.pos[] += 1
        d = Dict{String,Any}()
        skipws(r)
        if peekbyte(r) == UInt8('}')
            r.pos[] += 1
            return d
        end
        while true
            skipws(r)
            peekbyte(r) == UInt8('"') || error("expected string key at byte $(r.pos[])")
            k = read_string(r)
            skipws(r)
            expect(r, ":")
            d[k] = read_value(r)
            skipws(r)
            c = peekbyte(r)
            r.pos[] += 1
            c == UInt8('}') && return d
            c == UInt8(',') || error("expected ',' or '}' at byte $(r.pos[]-1)")
        end
    elseif b == UInt8('[')
        r.pos[] += 1
        a = Any[]
        skipws(r)
        if peekbyte(r) == UInt8(']')
            r.pos[] += 1
            return a
        end
        while true
            push!(a, read_value(r))
            skipws(r)
            c = peekbyte(r)
            r.pos[] += 1
            c == UInt8(']') && return a
            c == UInt8(',') || error("expected ',' or ']' at byte $(r.pos[]-1)")
        end
    elseif b == UInt8('"')
        return read_string(r)
    elseif b == UInt8('t')
        expect(r, "true")
        return true
    elseif b == UInt8('f')
        expect(r, "false")
        return false
    elseif b == UInt8('n')
        expect(r, "null")
        return nothing
    else
        return read_number(r)
    end
end

function read_number(r::Reader)
    start = r.pos[]
    while r.pos[] <= ncodeunits(r.s) && codeunit(r.s, r.pos[]) in UInt8.(collect("+-0123456789.eE"))
        r.pos[] += 1
    end
    t = r.s[start:r.pos[]-1]
    isempty(t) && error("invalid JSON value at byte $start")
    v = tryparse(Int, t)
    return v === nothing ? parse(Float64, t) : v
end

function read_hex4(r::Reader)
    r.pos[] + 3 <= ncodeunits(r.s) || error("truncated \\u escape")
    v = parse(UInt16, r.s[r.pos[]:r.pos[]+3]; base = 16)
    r.pos[] += 4
    return v
end

function read_string(r::Reader)
    r.pos[] += 1                       # opening quote
    io = IOBuffer()
    while true
        r.pos[] <= ncodeunits(r.s) || error("unterminated JSON string")
        b = codeunit(r.s, r.pos[])
        if b == UInt8('"')
            r.pos[] += 1
            return String(take!(io))
        elseif b == UInt8('\\')
            e = codeunit(r.s, r.pos[] + 1)
            r.pos[] += 2
            if e == UInt8('n')
                write(io, '\n')
            elseif e == UInt8('t')
                write(io, '\t')
            elseif e == UInt8('r')
                write(io, '\r')
            elseif e == UInt8('b')
                write(io, '\b')
            elseif e == UInt8('f')
                write(io, '\f')
            elseif e == UInt8('u')
                hi = read_hex4(r)
                if 0xd800 <= hi <= 0xdbff
                    expect(r, "\\u")
                    lo = read_hex4(r)
                    0xdc00 <= lo <= 0xdfff || error("invalid surrogate pair")
                    write(io, Char(0x10000 + ((UInt32(hi) - 0xd800) << 10) + (UInt32(lo) - 0xdc00)))
                else
                    write(io, Char(hi))
                end
            else
                write(io, Char(e))        # \" \\ \/
            end
        else
            write(io, b)
            r.pos[] += 1
        end
    end
end

function write_string(io::IO, s::AbstractString)
    write(io, '"')
    for c in s
        if !isvalid(c)
            # Output captured from user code is not guaranteed to be UTF-8;
            # the response must be.
            write(io, "\\ufffd")
        elseif c == '"'
            write(io, "\\\"")
        elseif c == '\\'
            write(io, "\\\\")
        elseif c == '\n'
            write(io, "\\n")
        elseif c == '\r'
            write(io, "\\r")
        elseif c == '\t'
            write(io, "\\t")
        elseif c < ' '
            write(io, "\\u", string(UInt32(c); base = 16, pad = 4))
        else
            write(io, c)
        end
    end
    write(io, '"')
end

write_json(io::IO, ::Nothing) = write(io, "null")
write_json(io::IO, b::Bool) = write(io, b ? "true" : "false")
write_json(io::IO, n::Integer) = write(io, string(n))
function write_json(io::IO, x::AbstractFloat)
    isfinite(x) || error("cannot encode non-finite number $x in JSON")
    write(io, string(Float64(x)))
end
write_json(io::IO, s::AbstractString) = write_string(io, s)
write_json(io::IO, s::Symbol) = write_string(io, String(s))
function write_json(io::IO, v::Union{AbstractVector,Tuple})
    write(io, '[')
    for (i, x) in enumerate(v)
        i > 1 && write(io, ',')
        write_json(io, x)
    end
    write(io, ']')
end
function write_json(io::IO, d::AbstractDict)
    write(io, '{')
    for (i, (k, v)) in enumerate(pairs(d))
        i > 1 && write(io, ',')
        write_string(io, string(k))
        write(io, ':')
        write_json(io, v)
    end
    write(io, '}')
end
write_json(x) = sprint(write_json, x)

end # module MiniJSON

using .MiniJSON

# ── Command line ─────────────────────────────────────────────────────────────

function parse_args(args)
    opts = Dict{String,String}()
    i = 1
    while i <= length(args)
        a = args[i]
        startswith(a, "--") && i < length(args) || error("unexpected argument: $a")
        opts[a[3:end]] = args[i+1]
        i += 2
    end
    for k in ("connect", "token", "parent-pid", "scratch")
        haskey(opts, k) || error("missing required argument --$k")
    end
    return opts
end

# ── Sessions ─────────────────────────────────────────────────────────────────

mutable struct Session
    mod::Module
    position::Union{Nothing,String}
end

const SESSIONS = Dict{String,Session}()

new_session(name::AbstractString) = Session(Module(Symbol("loom_", name), true, true), nothing)

# ── Figure defaults ──────────────────────────────────────────────────────────

const UNITS_PER_INCH = 96

function loaded_module(name::AbstractString)
    for (id, m) in Base.loaded_modules
        id.name == name && return m
    end
    return nothing
end

# Sets plotting-package defaults so that figures created without an explicit
# size match the cell's fig-width × fig-height. `done` records the packages
# already configured for the current cell, so a package loaded halfway
# through a cell is configured before the next statement while the user's own
# `default(...)` calls later in the same cell are not overwritten.
function apply_fig_defaults!(done::Set{String}, opts)
    w = round(Int, UNITS_PER_INCH * opts["fig-width"])
    h = round(Int, UNITS_PER_INCH * opts["fig-height"])
    png = opts["fig-format"] == "png"
    if !("Plots" in done) && (Plots = loaded_module("Plots")) !== nothing
        Base.invokelatest(Plots.default; size = (w, h), dpi = png ? opts["fig-dpi"] : 100)
        push!(done, "Plots")
    end
    if !("Makie" in done) && (Makie = loaded_module("Makie")) !== nothing
        Base.invokelatest(Makie.update_theme!; size = (w, h))
        push!(done, "Makie")
    end
    if !("CairoMakie" in done) && (CM = loaded_module("CairoMakie")) !== nothing
        Base.invokelatest(CM.activate!; px_per_unit = opts["fig-dpi"] / UNITS_PER_INCH)
        push!(done, "CairoMakie")
    end
    return nothing
end

# ── Output capture ───────────────────────────────────────────────────────────

const SCRATCH = Ref("")
const IMAGE_COUNTER = Ref(0)

const FORMAT_MIME = Dict("svg" => "image/svg+xml", "png" => "image/png", "pdf" => "application/pdf")

# Collects the outputs of one cell. stdout and stderr are redirected to files
# for the duration of the cell; `flush_streams!` moves whatever was written
# since the previous call into `stream` outputs, so that streams, log records,
# displays and values keep the order in which they happened.
mutable struct Capture
    outputs::Vector{Any}
    opts::Dict{String,Any}
    out::IOStream
    err::IOStream
    out_path::String
    err_path::String
    out_pos::Int
    err_pos::Int
end

function push_stream!(cap::Capture, name::String, text::String)
    isempty(text) && return
    last = isempty(cap.outputs) ? nothing : cap.outputs[end]
    if last !== nothing && last["type"] == "stream" && last["name"] == name
        last["text"] *= text
    else
        push!(cap.outputs, Dict{String,Any}("type" => "stream", "name" => name, "text" => text))
    end
end

function read_new(path::String, pos::Int)
    bytes = read(path)
    return String(bytes[pos+1:end]), length(bytes)
end

function flush_streams!(cap::Capture)
    flush(cap.out)
    flush(cap.err)
    t, cap.out_pos = read_new(cap.out_path, cap.out_pos)
    push_stream!(cap, "stdout", t)
    t, cap.err_pos = read_new(cap.err_path, cap.err_pos)
    push_stream!(cap, "stderr", t)
    return nothing
end

function finish_streams!(cap::Capture)
    flush_streams!(cap)
    for o in cap.outputs
        if o["type"] == "stream"
            o["text"] = chomp(o["text"])
        end
    end
    filter!(o -> !(o["type"] == "stream" && isempty(o["text"])), cap.outputs)
end

const CURRENT = Ref{Union{Nothing,Capture}}(nothing)

# Module of the session being executed; types defined in it print without a
# module prefix, as they would at the REPL.
const CURRENT_MODULE = Ref{Module}(Main)

function plain_repr(x)
    ctx = (:limit => true, :displaysize => (40, 80), :color => false, :module => CURRENT_MODULE[])
    return Base.invokelatest(sprint, show, MIME("text/plain"), x; context = ctx)
end

function next_image_path(ext::String)
    IMAGE_COUNTER[] += 1
    return joinpath(SCRATCH[], "julia-$(getpid())-$(IMAGE_COUNTER[]).$ext")
end

function mime_bundle(x, opts; rich::Bool)
    d = Dict{String,Any}("text/plain" => plain_repr(x))
    x isa AbstractString && (d["text/x-string"] = String(x))
    rich || return d
    typst = MIME("text/typst")
    if Base.invokelatest(showable, typst, x)
        d["text/typst"] = Base.invokelatest(sprint, show, typst, x)
        return d
    end
    order = unique([opts["fig-format"], "svg", "png", "pdf"])
    for fmt in order
        mime = MIME(FORMAT_MIME[fmt])
        Base.invokelatest(showable, mime, x) || continue
        path = next_image_path(fmt)
        open(path, "w") do io
            Base.invokelatest(show, io, mime, x)
        end
        filesize(path) > 0 || error("showing a $(typeof(x)) as $mime produced an empty file")
        d[FORMAT_MIME[fmt]] = Dict{String,Any}("file" => path)
        break
    end
    return d
end

# Receives `display(x)` calls made by user code. It is moved to the top of
# the display stack before every statement, because packages such as Plots
# push their own display when they load.
struct LoomDisplay <: AbstractDisplay end

const LOOM_DISPLAY = LoomDisplay()

function loom_display(@nospecialize(x))
    cap = CURRENT[]
    cap === nothing && return nothing   # setup replay produces no output
    flush_streams!(cap)
    push!(cap.outputs, Dict{String,Any}("type" => "display", "data" => mime_bundle(x, cap.opts; rich = true)))
    return nothing
end

Base.display(::LoomDisplay, @nospecialize(x)) = loom_display(x)
Base.display(::LoomDisplay, ::MIME, @nospecialize(x)) = loom_display(x)

function raise_display!()
    while LOOM_DISPLAY in Base.Multimedia.displays
        popdisplay(LOOM_DISPLAY)
    end
    pushdisplay(LOOM_DISPLAY)
end

# Turns log records into `message` / `warning` outputs, formatted as the REPL
# would print them.
struct LoomLogger <: AbstractLogger
    cap::Capture
end

Logging.min_enabled_level(::LoomLogger) = Logging.Info
Logging.shouldlog(::LoomLogger, args...) = true
Logging.catch_exceptions(::LoomLogger) = false

function Logging.handle_message(l::LoomLogger, level, message, _module, group, id, file, line; kwargs...)
    flush_streams!(l.cap)
    buf = IOBuffer()
    console = ConsoleLogger(IOContext(buf, :color => false), Logging.Debug)
    Logging.handle_message(console, level, message, _module, group, id, file, line; kwargs...)
    text = chomp(String(take!(buf)))
    kind = level >= Logging.Warn ? "warning" : "message"
    push!(l.cap.outputs, Dict{String,Any}("type" => kind, "text" => text))
    return nothing
end

function error_text(e, bt)
    frames = stacktrace(bt)
    # Frames below the cell's top-level scope belong to the kernel.
    i = findfirst(f -> f.func === Symbol("top-level scope"), frames)
    frames = frames[1:something(i, length(frames))]
    return sprint() do io
        showerror(IOContext(io, :color => false, :limit => true), e, frames; backtrace = true)
    end
end

# Runs `f()` with stdout/stderr redirected into `cap` and the loom logger
# and display installed.
function with_capture(f, cap::Capture)
    CURRENT[] = cap
    try
        redirect_stdout(cap.out) do
            redirect_stderr(cap.err) do
                with_logger(LoomLogger(cap)) do
                    f()
                end
            end
        end
    finally
        CURRENT[] = nothing
        close(cap.out)
        close(cap.err)
        rm(cap.out_path; force = true)
        rm(cap.err_path; force = true)
    end
end

function new_capture(opts)
    out_path, out = mktemp(SCRATCH[])
    err_path, err = mktemp(SCRATCH[])
    return Capture(Any[], opts, out, err, out_path, err_path, 0, 0)
end

# ── Block mode ───────────────────────────────────────────────────────────────

# Byte offset of the first character of `code[first:last]` that is neither
# whitespace nor part of a comment. Node ranges from the bundled parser can
# start with leading trivia (on Julia 1.10, the previous line's trailing
# comment), so this is where a statement really starts.
function first_token_byte(code::String, first::Int, last::Int)
    i = first
    while i <= last
        c = code[i]
        if isspace(c)
            i = nextind(code, i)
        elseif c == '#' && i < last && code[nextind(code, i)] == '='
            depth = 1
            i = nextind(code, nextind(code, i))
            while i <= last && depth > 0
                if startswith(SubString(code, i), "#=")
                    depth += 1
                    i += 2
                elseif startswith(SubString(code, i), "=#")
                    depth -= 1
                    i += 2
                else
                    i = nextind(code, i)
                end
            end
        elseif c == '#'
            while i <= last && code[i] != '\n'
                i = nextind(code, i)
            end
        else
            return i
        end
    end
    return first
end

line_of(code::String, byte::Int) = count(==(UInt8('\n')), view(codeunits(code), 1:byte-1)) + 1

struct Group
    nodes::Vector{Any}
    first_line::Int
    last_line::Int
end

# Groups top-level nodes into source outputs. A group ends on the last line of
# its last statement; statements that start on a line already used by the
# previous group join that group. Lines between groups belong to the
# following group and lines after the last statement to the last group.
function group_statements(code::String, nodes)
    groups = Group[]
    for n in nodes
        fb = first_token_byte(code, JS.first_byte(n), JS.last_byte(n))
        l1 = line_of(code, fb)
        l2 = line_of(code, JS.last_byte(n))
        if !isempty(groups) && l1 <= groups[end].last_line
            g = groups[end]
            groups[end] = Group(push!(g.nodes, n), g.first_line, max(g.last_line, l2))
        else
            push!(groups, Group(Any[n], l1, l2))
        end
    end
    return groups
end

function exec_block(sess::Session, code::String, opts, file::String, line::Int)
    cap = new_capture(opts)
    failed = false
    lines = split(code, '\n'; keepempty = true)
    tree = try
        JS.parseall(JS.SyntaxNode, code; filename = file, first_line = line)
    catch e
        e isa JS.ParseError || rethrow()
        close(cap.out); close(cap.err)
        rm(cap.out_path; force = true); rm(cap.err_path; force = true)
        msg = sprint(showerror, e)
        return true, Any[Dict{String,Any}("type" => "source", "code" => code),
                         Dict{String,Any}("type" => "error", "text" => msg)]
    end
    nodes = collect(something(JS.children(tree), Any[]))
    if isempty(nodes)
        close(cap.out); close(cap.err)
        rm(cap.out_path; force = true); rm(cap.err_path; force = true)
        return false, Any[Dict{String,Any}("type" => "source", "code" => code)]
    end
    groups = group_statements(code, nodes)
    last_node = nodes[end]
    configured = Set{String}()
    with_capture(cap) do
        prev_end = 0
        for (gi, g) in enumerate(groups)
            stop = gi == length(groups) ? length(lines) : g.last_line
            src = join(lines[prev_end+1:stop], '\n')
            prev_end = stop
            flush_streams!(cap)
            push!(cap.outputs, Dict{String,Any}("type" => "source", "code" => src))
            for n in g.nodes
                apply_fig_defaults!(configured, opts)
                raise_display!()
                stmt_line = line - 1 + line_of(code, first_token_byte(code, JS.first_byte(n), JS.last_byte(n)))
                ex = Expr(:toplevel, LineNumberNode(stmt_line, Symbol(file)), Expr(n))
                value = try
                    Core.eval(sess.mod, ex)
                catch e
                    flush_streams!(cap)
                    push!(cap.outputs, Dict{String,Any}("type" => "error", "text" => error_text(e, catch_backtrace())))
                    failed = true
                    break
                end
                suppressed = endswith(rstrip(code[JS.first_byte(n):JS.last_byte(n)]), ';')
                if value !== nothing && !suppressed
                    flush_streams!(cap)
                    is_last = n === last_node
                    push!(cap.outputs, Dict{String,Any}(
                        "type" => "value",
                        "repl_only" => !is_last,
                        "data" => mime_bundle(value, opts; rich = is_last),
                    ))
                end
            end
            failed && break
        end
        finish_streams!(cap)
    end
    return failed, cap.outputs
end

# ── Inline mode ──────────────────────────────────────────────────────────────

function exec_inline(sess::Session, code::String, opts, file::String, line::Int)
    cap = new_capture(opts)
    failed = false
    with_capture(cap) do
        try
            tree = JS.parseall(JS.SyntaxNode, code; filename = file, first_line = line)
            value = Core.eval(sess.mod, Expr(tree))
            d = Dict{String,Any}("text/plain" => Base.invokelatest(sprint, print, value))
            typst = MIME("text/typst")
            if Base.invokelatest(showable, typst, value)
                d["text/typst"] = Base.invokelatest(sprint, show, typst, value)
            end
            push!(cap.outputs, Dict{String,Any}("type" => "value", "repl_only" => false, "data" => d))
        catch e
            msg = e isa JS.ParseError ? sprint(showerror, e) : error_text(e, catch_backtrace())
            push!(cap.outputs, Dict{String,Any}("type" => "error", "text" => msg))
            failed = true
        end
    end
    # Inline mode returns only the value or the error; printed text is dropped.
    filter!(o -> o["type"] in ("value", "error"), cap.outputs)
    return failed, cap.outputs
end

# ── Requests ─────────────────────────────────────────────────────────────────

protocol_error(msg) = Dict{String,Any}("ok" => false, "error" => msg)

function required(req, key, T)
    haskey(req, key) || error("request is missing field '$key'")
    v = req[key]
    v isa T || error("field '$key' must be of type $T, got $(typeof(v))")
    return v
end

function handle_reset(req)
    name = required(req, "session", String)
    position = required(req, "position", String)
    setup = required(req, "setup", Vector)
    sess = new_session(name)
    SESSIONS[name] = sess
    for (i, code) in enumerate(setup)
        code isa String || error("setup entries must be strings")
        try
            Base.invokelatest(include_string, sess.mod, code, "setup[$i]")
        catch e
            err = e isa LoadError ? e.error : e
            text = "in setup code $i: " * error_text(err, catch_backtrace())
            return Dict{String,Any}("ok" => true, "failed" => true,
                                    "outputs" => Any[Dict{String,Any}("type" => "error", "text" => text)])
        end
    end
    sess.position = position
    return Dict{String,Any}("ok" => true, "failed" => false, "outputs" => Any[])
end

function handle_exec(req)
    name = required(req, "session", String)
    after = required(req, "after", String)
    id = required(req, "id", String)
    mode = required(req, "mode", String)
    code = required(req, "code", String)
    raw = required(req, "options", AbstractDict)
    opts = Dict{String,Any}(
        "fig-width" => Float64(required(raw, "fig-width", Real)),
        "fig-height" => Float64(required(raw, "fig-height", Real)),
        "fig-format" => required(raw, "fig-format", String),
        "fig-dpi" => Float64(required(raw, "fig-dpi", Real)),
    )
    haskey(FORMAT_MIME, opts["fig-format"]) || error("unknown fig-format '$(opts["fig-format"])'")
    file = get(req, "file", "cell")
    line = get(req, "line", 1)
    sess = get(SESSIONS, name, nothing)
    sess === nothing || (CURRENT_MODULE[] = sess.mod)
    have = sess === nothing ? "<no session>" : something(sess.position, "null")
    if sess === nothing || sess.position != after
        return protocol_error("position mismatch: session '$name' is at $have, request expects $after")
    end
    failed, outputs = if mode == "block"
        exec_block(sess, code, opts, file, line)
    elseif mode == "inline"
        exec_inline(sess, code, opts, file, line)
    else
        error("unknown mode '$mode'")
    end
    sess.position = id
    return Dict{String,Any}("ok" => true, "failed" => failed, "outputs" => outputs)
end

function handle(req, token)
    op = required(req, "op", String)
    if op == "hello"
        return Dict{String,Any}("ok" => true, "protocol" => PROTOCOL_VERSION, "language" => "julia",
                                "version" => string(VERSION), "pid" => getpid())
    elseif op == "reset"
        return handle_reset(req)
    elseif op == "exec"
        return handle_exec(req)
    elseif op == "shutdown"
        return Dict{String,Any}("ok" => true)
    else
        return protocol_error("unknown op '$op'")
    end
end

# ── Server ───────────────────────────────────────────────────────────────────

function parent_alive(pid::Integer)
    ccall(:kill, Cint, (Cint, Cint), pid, 0) == 0 && return true
    return Libc.errno() == Libc.EPERM
end

# Runs on the interactive thread pool when one exists, so that the kernel
# also exits while a cell is computing.
function start_watchdog(pid::Integer)
    check() = while true
        sleep(2)
        parent_alive(pid) || ccall(:_exit, Cvoid, (Cint,), 0)
    end
    if isdefined(Threads, :nthreads) && Threads.nthreads(:interactive) > 0
        Threads.@spawn :interactive check()
    else
        @async check()
    end
end

# Serves requests on the single connection to loom; returns when loom closes
# it (or on an invalid token), after which the kernel exits.
function serve(port::Int, token::String)
    conn = connect(Sockets.IPv4(127, 0, 0, 1), port)
    write(conn, write_json(Dict("token" => token)), '\n')
    flush(conn)
    @info "loom julia kernel connected" port pid = getpid() julia = VERSION
    try
        while true
            line = readline(conn)
            if isempty(line)
                eof(conn) && return
                continue
            end
            local req = nothing
            resp = try
                req = parse_json(line)
                req isa AbstractDict || error("request is not a JSON object")
                if get(req, "token", nothing) != token
                    write(conn, write_json(protocol_error("invalid token")), '\n')
                    return
                end
                Base.invokelatest(handle, req, token)
            catch e
                @error "internal kernel error" exception = (e, catch_backtrace())
                protocol_error("internal kernel error: " * sprint(showerror, e, catch_backtrace()))
            end
            write(conn, write_json(resp), '\n')
            flush(conn)
            req isa AbstractDict && get(req, "op", nothing) == "shutdown" && return
        end
    catch e
        e isa Base.IOError || e isa EOFError || rethrow()
    finally
        close(conn)
    end
end

function main(args)
    opts = parse_args(args)
    SCRATCH[] = opts["scratch"]
    mkpath(SCRATCH[])
    # Plots.jl's GR backend opens a window unless told to draw nowhere.
    ENV["GKSwstype"] = "nul"
    start_watchdog(parse(Int, opts["parent-pid"]))
    serve(parse(Int, opts["connect"]), opts["token"])
    exit(0)
end

end # module LoomKernel

LoomKernel.main(ARGS)
