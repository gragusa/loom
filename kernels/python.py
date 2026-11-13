"""Python kernel for loom.

Implements version 1 of the loom kernel protocol (kernels/PROTOCOL.md in the
loom repository): the kernel connects to loom on 127.0.0.1, identifies itself
with the shared token, then answers one JSON request per line with one JSON
response per line, executing code in named sessions.

Only the standard library is required.  matplotlib is used when user code has
loaded it; the kernel never imports it on its own.

Usage:
    python3 python.py --connect P --token T --parent-pid PID --scratch DIR
"""

import os

# matplotlib reads MPLBACKEND when it is first imported, which is always after
# this point.  A non-interactive backend keeps figures off the screen; child
# processes inherit the setting.
os.environ["MPLBACKEND"] = "Agg"

import __future__
import argparse
import ast
import base64
import bisect
import builtins
import hmac
import io
import itertools
import json
import linecache
import math
import platform
import re
import socket
import sys
import threading
import time
import traceback
import types
import warnings

PROTOCOL_VERSION = 1
PARENT_CHECK_INTERVAL = 2.0

# Kernel messages go to the process's original stderr (the kernel log), never
# into a cell's captured output.
_LOG = sys.stderr
_KERNEL_GLOBALS = globals()

_IMAGE_FORMATS = ("svg", "png", "pdf")
_IMAGE_MIME = {"svg": "image/svg+xml", "png": "image/png", "pdf": "application/pdf"}
_IMAGE_REPR = {"svg": "_repr_svg_", "png": "_repr_png_", "pdf": "_repr_pdf_"}

# Python's parser ends a line at "\r\n", "\r" or "\n"; AST line numbers count
# lines this way.
_PY_NEWLINE = re.compile(r"\r\n|\r|\n")
_SURROGATE = re.compile("[\ud800-\udfff]")

_BACKEND_MODULE = "loom_python_kernel_backend"
_PENDING_FIGURE = "pending-figure"

_sessions = {}
_scratch = None
_current = None  # the _Cell collecting output, or None between cells
_backend_checked = False
_image_numbers = itertools.count(1)
_source_numbers = itertools.count(1)


def _log(message):
    print(f"[loom python kernel {os.getpid()}] {message}", file=_LOG, flush=True)


class RequestError(Exception):
    """A protocol-level problem; answered with ok: false."""


class _UserError(Exception):
    """Carries an exception raised by user code or by objects it returned."""

    def __init__(self, exc):
        super().__init__(exc)
        self.exc = exc


class _UserCode:
    """Context manager marking code whose exceptions belong to the user.

    Everything raised inside, including SystemExit and KeyboardInterrupt, is
    re-raised as _UserError; exceptions raised elsewhere are kernel errors.
    """

    def __enter__(self):
        return self

    def __exit__(self, kind, exc, tb):
        if exc is None or isinstance(exc, _UserError):
            return False
        raise _UserError(exc) from None


_USER_CODE = _UserCode()


# ── Output capture ──────────────────────────────────────────────────────────


class _OutputRouter(io.TextIOBase):
    """Replacement for sys.stdout / sys.stderr.

    Installed once for the life of the process, so that objects which keep a
    reference to sys.stdout (logging handlers, for example) still reach the
    cell that is running when they write.  Between cells, text goes to the
    original stream.
    """

    def __init__(self, name, fallback):
        super().__init__()
        self.name = name
        self._fallback = fallback

    @property
    def encoding(self):
        return "utf-8"

    @property
    def errors(self):
        return "strict"

    def writable(self):
        return True

    def isatty(self):
        return False

    def fileno(self):
        return self._fallback.fileno()

    def write(self, text):
        if not isinstance(text, str):
            raise TypeError(f"write() argument must be str, not {type(text).__name__}")
        cell = _current
        if cell is None:
            return self._fallback.write(text)
        cell.add_stream(self.name, text)
        return len(text)

    def flush(self):
        if _current is None:
            self._fallback.flush()


_original_showwarning = warnings.showwarning


def _showwarning(message, category, filename, lineno, file=None, line=None):
    cell = _current
    if cell is None or file is not None:
        _original_showwarning(message, category, filename, lineno, file, line)
        return
    cell.add({"type": "warning", "text": f"{category.__name__}: {message}"})


def _reset_warning_registries():
    # Bumping the warning filters' version invalidates every module's
    # __warningregistry__, so a "default" or "once" warning appears in each
    # cell that triggers it instead of once per kernel process: a cell's
    # output does not depend on which cells this process ran before.
    mutated = getattr(warnings, "_filters_mutated", None)
    if mutated is not None:
        mutated()


def _format_exception(exc):
    """Traceback text for a user error, without the kernel's own frames."""
    tb = exc.__traceback__
    while tb is not None and tb.tb_frame.f_globals is _KERNEL_GLOBALS:
        tb = tb.tb_next
    if isinstance(exc, SyntaxError) and exc.text is None and exc.filename and exc.lineno:
        # Code compiled from an AST carries no source text; linecache has it.
        text = linecache.getline(exc.filename, exc.lineno)
        if text:
            exc.text = text
    return "".join(traceback.format_exception(type(exc), exc, tb)).rstrip("\n")


def _register_source(filename, first_line, code):
    """Make `code` the text of lines first_line, ... of `filename` for linecache.

    Tracebacks, inspect.getsource and pdb then show the code that ran.  Cells
    from the same document file share one entry.
    """
    entry = linecache.cache.get(filename)
    lines = list(entry[2]) if entry is not None and len(entry) == 4 else []
    cell_lines = [line + "\n" for line in _PY_NEWLINE.split(code)]
    start = first_line - 1
    if len(lines) < start:
        lines.extend(["\n"] * (start - len(lines)))
    lines[start:start + len(cell_lines)] = cell_lines
    # An mtime of None tells linecache.checkcache not to re-read the entry
    # from disk or evict it.
    linecache.cache[filename] = (sum(map(len, lines)), None, lines, filename)


# ── Sessions ────────────────────────────────────────────────────────────────


class _Session:
    def __init__(self):
        self.globals = {"__name__": "__main__", "__builtins__": builtins}
        self.position = None
        self.compiler_flags = 0

    def compile(self, source, filename, mode, flags=0):
        """compile() that keeps __future__ imports in force for later code."""
        code = compile(source, filename, mode, self.compiler_flags | flags, dont_inherit=True)
        if isinstance(code, types.CodeType):
            for name in __future__.all_feature_names:
                feature = getattr(__future__, name).compiler_flag
                if code.co_flags & feature:
                    self.compiler_flags |= feature
        return code


# ── Block-mode source partition ─────────────────────────────────────────────


def _partition(code, body, line_shift):
    r"""Split `code` into chunks of whole lines, one per group of statements.

    Returns a list of (chunk, [(statement, suppressed), ...]).  Statements
    that share a line form one group.  Lines without a statement belong to the
    next group, trailing ones to the last.  "\n".join of the chunks is `code`.
    `suppressed` is true when the statement is followed by ";".  AST line
    numbers are those of `code` plus `line_shift`.
    """
    # Chunks are made of "\n"-separated lines, the unit loom joins them with.
    nl_starts = [0]
    i = code.find("\n")
    while i != -1:
        nl_starts.append(i + 1)
        i = code.find("\n", i + 1)
    py_lines = _PY_NEWLINE.split(code)
    py_starts = [0] + [m.end() for m in _PY_NEWLINE.finditer(code)]

    def offset(lineno, col):
        # AST columns count UTF-8 bytes.
        k = lineno - 1 - line_shift
        text = py_lines[k]
        if not text.isascii():
            col = len(text.encode("utf-8")[:col].decode("utf-8"))
        return py_starts[k] + col

    def line_of(pos):
        return bisect.bisect_right(nl_starts, pos) - 1

    groups = []  # [first line, last line, [(statement, suppressed), ...]]
    for stmt in body:
        # A decorated definition's own position is that of "def" or "class".
        nodes = [stmt] + list(getattr(stmt, "decorator_list", ()))
        start = min(offset(n.lineno, n.col_offset) for n in nodes)
        end = offset(stmt.end_lineno, stmt.end_col_offset)
        first, last = line_of(start), line_of(end - 1)
        item = (stmt, _followed_by_semicolon(code, end))
        if groups and first <= groups[-1][1]:
            groups[-1][1] = max(groups[-1][1], last)
            groups[-1][2].append(item)
        else:
            groups.append([first, last, [item]])

    lines = code.split("\n")
    chunks = []
    begin = 0
    for index, (_, last, items) in enumerate(groups):
        stop = len(lines) - 1 if index == len(groups) - 1 else last
        chunks.append(("\n".join(lines[begin:stop + 1]), items))
        begin = stop + 1
    return chunks


def _followed_by_semicolon(code, pos):
    """True when the first token after `pos` is ";"."""
    n = len(code)
    while pos < n:
        c = code[pos]
        if c in " \t\f":
            pos += 1
        elif c == "\\":  # explicit line joining
            if code.startswith("\r\n", pos + 1):
                pos += 3
            elif pos + 1 < n and code[pos + 1] in "\r\n":
                pos += 2
            else:
                return False
        else:
            return c == ";"
    return False


# ── matplotlib ──────────────────────────────────────────────────────────────


def _open_figures():
    """Figures managed by pyplot, in figure-number order."""
    helpers = sys.modules.get("matplotlib._pylab_helpers")
    if helpers is None:
        return []
    managers = sorted(helpers.Gcf.get_all_fig_managers(), key=lambda m: m.num)
    return [m.canvas.figure for m in managers]


def _close_figure(fig):
    sys.modules["matplotlib._pylab_helpers"].Gcf.destroy_fig(fig)


def _close_all_figures():
    helpers = sys.modules.get("matplotlib._pylab_helpers")
    if helpers is not None:
        helpers.Gcf.destroy_all()


def _pyplot_show(*args, **kwargs):
    """pyplot.show() for the kernel's backend: display the open figures now.

    A loop that draws and shows a figure per iteration then yields one image
    per iteration.
    """
    cell = _current
    figures = _open_figures()
    try:
        for fig in figures:
            if cell is not None:
                cell.forget_figure(fig)
                cell.add(_figure_display(fig, cell.options["fig-format"]))
    finally:
        for fig in figures:
            _close_figure(fig)


def _install_backend(mpl):
    """Switch matplotlib from Agg to an Agg-based backend whose show() displays.

    Plain Agg ignores plt.show() (with a warning, except on headless Linux).
    The backend is selected in-process rather than through MPLBACKEND so that
    child processes keep plain Agg.
    """
    global _backend_checked
    if _backend_checked:
        return
    _backend_checked = True
    if str(mpl.get_backend()).lower() != "agg":
        return  # user code chose a backend; keep it
    from matplotlib.backend_bases import FigureManagerBase
    from matplotlib.backends.backend_agg import FigureCanvasAgg

    backend = types.ModuleType(_BACKEND_MODULE)
    backend.FigureCanvas = FigureCanvasAgg
    backend.FigureManager = FigureManagerBase
    backend.show = _pyplot_show
    sys.modules[_BACKEND_MODULE] = backend
    mpl.use("module://" + _BACKEND_MODULE)


def _prepare_matplotlib(options):
    """Apply the cell's figure options if matplotlib is loaded; report whether it is."""
    mpl = sys.modules.get("matplotlib")
    if mpl is None:
        return False
    _install_backend(mpl)
    mpl.rcParams["figure.figsize"] = (options["fig-width"], options["fig-height"])
    mpl.rcParams["savefig.dpi"] = options["fig-dpi"]
    return True


class _PyplotImportHook:
    """Import finder that prepares matplotlib just before pyplot is first imported.

    pyplot is often first imported inside the statement that draws a figure
    (pandas' DataFrame.plot does so).  Preparing matplotlib at that moment
    gives the figure the cell's size and the kernel's backend; the check at
    the next statement boundary would come too late for it.
    """

    def find_spec(self, fullname, path=None, target=None):
        if fullname != "matplotlib.pyplot":
            return None
        sys.meta_path.remove(self)
        cell = _current
        if cell is not None:
            cell.prepare_matplotlib()
        else:
            _install_backend(sys.modules["matplotlib"])
        return None  # the regular finders locate pyplot


def _is_matplotlib_object(value):
    """True for artists (figures, axes, lines, ...), colorbars, and containers of them."""
    artist = sys.modules.get("matplotlib.artist")
    if artist is None:
        return False
    kinds = (artist.Artist,)
    colorbar = sys.modules.get("matplotlib.colorbar")
    if colorbar is not None:
        kinds += (colorbar.Colorbar,)
    return _all_instances(value, kinds, 0)


def _all_instances(value, kinds, depth):
    if isinstance(value, kinds):
        return True
    if depth >= 3:
        return False
    if isinstance(value, (list, tuple)):
        return len(value) > 0 and all(_all_instances(v, kinds, depth + 1) for v in value)
    numpy = sys.modules.get("numpy")
    if numpy is not None and isinstance(value, numpy.ndarray) and value.dtype == object:
        return value.size > 0 and all(_all_instances(v, kinds, depth + 1) for v in value.flat)
    return False


def _unmanaged_figure(value):
    """`value` if it is a matplotlib Figure that pyplot does not manage, else None."""
    figure = sys.modules.get("matplotlib.figure")
    if figure is None or not isinstance(value, figure.Figure):
        return None
    if any(f is value for f in _open_figures()):
        return None
    return value


# ── MIME bundles ────────────────────────────────────────────────────────────


def _image_path(fmt):
    return os.path.join(_scratch, f"python-{os.getpid()}-{next(_image_numbers)}.{fmt}")


def _figure_display(fig, fmt):
    path = _image_path(fmt)
    fig.savefig(path, format=fmt)
    return {"type": "display", "data": {_IMAGE_MIME[fmt]: {"file": path}}}


def _repr_method(value, name):
    # Looked up on the type: classes themselves, and proxies whose
    # __getattr__ answers every name, are not asked for representations.
    if isinstance(value, type) or getattr(type(value), name, None) is None:
        return None
    return getattr(value, name)


def _call_repr(value, name):
    method = _repr_method(value, name)
    if method is None:
        return None
    result = method()
    # IPython convention: a (data, metadata) pair.
    if isinstance(result, tuple) and len(result) == 2 and isinstance(result[1], dict):
        result = result[0]
    return result


def _typst_repr(value):
    typst = _call_repr(value, "_repr_typst_")
    if typst is not None and not isinstance(typst, str):
        raise TypeError(
            f"{type(value).__name__}._repr_typst_() returned "
            f"{type(typst).__name__}, expected str")
    return typst


def _image_bytes(value, fmt, data):
    if isinstance(data, (bytes, bytearray, memoryview)):
        return bytes(data)
    if isinstance(data, str):
        # SVG is text; PNG and PDF given as text are base64 (IPython convention).
        return data.encode("utf-8") if fmt == "svg" else base64.b64decode(data)
    raise TypeError(
        f"{type(value).__name__}.{_IMAGE_REPR[fmt]}() returned "
        f"{type(data).__name__}, expected bytes or str")


def _rich_data(value, preferred):
    """text/typst, or else at most one image, for `value`."""
    typst = _typst_repr(value)
    if typst is not None:
        return {"text/typst": typst}
    for fmt in (preferred,) + tuple(f for f in _IMAGE_FORMATS if f != preferred):
        data = _call_repr(value, _IMAGE_REPR[fmt])
        if data is None:
            continue
        path = _image_path(fmt)
        with open(path, "wb") as fh:
            fh.write(_image_bytes(value, fmt, data))
        return {_IMAGE_MIME[fmt]: {"file": path}}
    return {}


def _value_output(value, rich, preferred):
    data = {"text/plain": repr(value)}
    if isinstance(value, str):
        data["text/x-string"] = str.__str__(value)
    if rich:
        data.update(_rich_data(value, preferred))
    return {"type": "value", "repl_only": not rich, "data": data}


# ── Cells ───────────────────────────────────────────────────────────────────


class _Cell:
    """Outputs of one block-mode exec request.

    A figure is displayed after the last statement that modified it, in its
    state at the end of the cell: a placeholder is appended to the outputs
    whenever a statement leaves a figure stale (modified since last checked)
    and replaced by the image when the cell ends.  Building a figure over
    several statements therefore yields one image.  Figures closed by user
    code are not displayed; plt.show() displays the open figures at once.
    """

    def __init__(self, options):
        self.options = options
        self.outputs = []
        self.failed = False
        self._pending = {}  # id(figure) -> its placeholder output
        self._matplotlib_ready = False

    def add(self, output):
        self.outputs.append(output)

    def add_stream(self, name, text):
        last = self.outputs[-1] if self.outputs else None
        if last is not None and last["type"] == "stream" and last["name"] == name:
            last["text"] += text
        else:
            self.outputs.append({"type": "stream", "name": name, "text": text})

    def fail(self, exc):
        self.failed = True
        self.add({"type": "error", "text": _format_exception(exc)})

    def forget_figure(self, fig):
        placeholder = self._pending.pop(id(fig), None)
        if placeholder is not None:
            placeholder["type"] = None
            del placeholder["figure"]

    def track_figures(self):
        figures = _open_figures()
        open_ids = {id(f) for f in figures}
        for key in [k for k in self._pending if k not in open_ids]:
            placeholder = self._pending.pop(key)
            placeholder["type"] = None
            del placeholder["figure"]
        for fig in figures:
            if fig.stale:
                self.forget_figure(fig)
                placeholder = {"type": _PENDING_FIGURE, "figure": fig}
                self.outputs.append(placeholder)
                self._pending[id(fig)] = placeholder
                fig.stale = False

    def run(self, session, code, filename, first_line):
        try:
            with _USER_CODE:
                tree = session.compile("\n" * (first_line - 1) + code, filename, "exec",
                                       ast.PyCF_ONLY_AST)
        except _UserError as err:
            self.add({"type": "source", "code": code})
            self.fail(err.exc)
            return
        body = tree.body
        if not body:
            if code:
                self.add({"type": "source", "code": code})
            return
        for chunk, items in _partition(code, body, first_line - 1):
            self.add({"type": "source", "code": chunk})
            for stmt, suppressed in items:
                if not self._run_statement(session, stmt, filename, stmt is body[-1], suppressed):
                    return

    def prepare_matplotlib(self):
        if not self._matplotlib_ready:
            self._matplotlib_ready = _prepare_matplotlib(self.options)

    def _run_statement(self, session, stmt, filename, is_last, suppressed):
        try:
            with _USER_CODE:
                self.prepare_matplotlib()
                if isinstance(stmt, ast.Expr):
                    code = session.compile(ast.Expression(body=stmt.value), filename, "eval")
                    value = eval(code, session.globals)
                    if value is not None and not suppressed:
                        self._show_value(value, is_last)
                else:
                    code = session.compile(ast.Module(body=[stmt], type_ignores=[]),
                                           filename, "exec")
                    exec(code, session.globals)
                self.track_figures()
        except _UserError as err:
            self.fail(err.exc)
            return False
        return True

    def _show_value(self, value, is_last):
        fmt = self.options["fig-format"]
        if _is_matplotlib_object(value):
            # The figure is displayed on its own.  A figure pyplot does not
            # manage would otherwise never be shown.
            fig = _unmanaged_figure(value)
            if fig is not None and is_last:
                self.add(_figure_display(fig, fmt))
            return
        self.add(_value_output(value, is_last, fmt))

    def finish(self):
        """Replace figure placeholders by images."""
        open_ids = {id(f) for f in _open_figures()}
        for output in list(self.outputs):
            if output["type"] != _PENDING_FIGURE:
                continue
            fig = output.pop("figure")
            output["type"] = None
            if id(fig) not in open_ids:
                continue
            try:
                with _USER_CODE:
                    display = _figure_display(fig, self.options["fig-format"])
            except _UserError as err:
                self.fail(err.exc)
                open_ids = set()  # stop at the first failure
                continue
            output.update(display)


def _finalize(outputs):
    """Merge consecutive streams, drop blank ones, strip trailing newlines."""
    kept = [o for o in outputs if o["type"] is not None]
    kept = _merge_streams(kept)
    kept = [o for o in kept if not (o["type"] == "stream" and not o["text"].strip("\n"))]
    kept = _merge_streams(kept)
    for output in kept:
        if output["type"] in ("stream", "warning", "error"):
            output["text"] = output["text"].rstrip("\n")
    return kept


def _merge_streams(outputs):
    merged = []
    for output in outputs:
        last = merged[-1] if merged else None
        if (output["type"] == "stream" and last is not None and last["type"] == "stream"
                and last["name"] == output["name"]):
            merged[-1] = dict(last, text=last["text"] + output["text"])
        else:
            merged.append(output)
    return merged


def _exec_block(session, code, filename, first_line, options):
    global _current
    cell = _Cell(options)
    _register_source(filename, first_line, code)
    _reset_warning_registries()
    _current = cell
    try:
        cell.run(session, code, filename, first_line)
        cell.finish()
    finally:
        _current = None
        _close_all_figures()
    return cell.failed, _finalize(cell.outputs)


def _exec_inline(session, code, filename, first_line):
    _register_source(filename, first_line, code)
    expression = code.lstrip()
    skipped = len(_PY_NEWLINE.findall(code, 0, len(code) - len(expression)))
    source = "\n" * (first_line - 1 + skipped) + expression
    try:
        with _USER_CODE:
            value = eval(session.compile(source, filename, "eval"), session.globals)
            data = {"text/plain": str(value)}
            if isinstance(value, str):
                data["text/x-string"] = str.__str__(value)
            typst = _typst_repr(value)
            if typst is not None:
                data["text/typst"] = typst
    except _UserError as err:
        return True, [{"type": "error", "text": _format_exception(err.exc)}]
    finally:
        _close_all_figures()
    return False, [{"type": "value", "repl_only": False, "data": data}]


def _run_setup(session, sources):
    try:
        for source in sources:
            filename = f"<setup-{next(_source_numbers)}>"
            _register_source(filename, 1, source)
            try:
                with _USER_CODE:
                    exec(session.compile(source, filename, "exec"), session.globals)
            except _UserError as err:
                return [{"type": "error", "text": _format_exception(err.exc)}]
        return []
    finally:
        _close_all_figures()


# ── Requests ────────────────────────────────────────────────────────────────


def _malformed(message):
    return RequestError(f"malformed request: {message}")


def _string(req, key):
    value = req.get(key)
    if not isinstance(value, str):
        raise _malformed(f"'{key}' must be a string")
    return value


def _position(req, key):
    if key not in req:
        raise _malformed(f"missing '{key}'")
    value = req[key]
    if value is not None and not isinstance(value, str):
        raise _malformed(f"'{key}' must be a string or null")
    return value


def _show_position(position):
    return "null" if position is None else position


def _options(req):
    options = req.get("options")
    if not isinstance(options, dict):
        raise _malformed("'options' must be an object")
    parsed = {}
    for key in ("fig-width", "fig-height", "fig-dpi"):
        value = options.get(key)
        if (isinstance(value, bool) or not isinstance(value, (int, float))
                or not math.isfinite(value) or value <= 0):
            raise _malformed(f"options '{key}' must be a positive number")
        parsed[key] = float(value)
    fmt = options.get("fig-format")
    if fmt not in _IMAGE_FORMATS:
        raise _malformed(f"options 'fig-format' must be one of {', '.join(_IMAGE_FORMATS)}")
    parsed["fig-format"] = fmt
    return parsed


def _location(req, prefix):
    """Filename and first line for compiling the request's code."""
    file = req.get("file")
    line = req.get("line")
    if file is not None and (not isinstance(file, str) or not file):
        raise _malformed("'file' must be a non-empty string")
    if line is not None and (isinstance(line, bool) or not isinstance(line, int) or line < 1):
        raise _malformed("'line' must be a positive integer")
    if file is None:
        # Without a file, a line number would point into nothing.
        return f"<{prefix}-{next(_source_numbers)}>", 1
    return file, 1 if line is None else line


def _op_hello(req):
    return {"ok": True, "protocol": PROTOCOL_VERSION, "language": "python",
            "version": platform.python_version(), "pid": os.getpid()}


def _op_reset(req):
    name = _string(req, "session")
    position = _position(req, "position")
    setup = req.get("setup", [])
    if not isinstance(setup, list) or not all(isinstance(s, str) for s in setup):
        raise _malformed("'setup' must be a list of strings")
    session = _Session()
    _sessions[name] = session
    outputs = _run_setup(session, setup)
    session.position = None if outputs else position
    return {"ok": True, "failed": bool(outputs), "outputs": outputs}


def _op_exec(req):
    name = _string(req, "session")
    after = _position(req, "after")
    cell_id = _string(req, "id")
    mode = req.get("mode")
    if mode not in ("block", "inline"):
        raise _malformed("'mode' must be \"block\" or \"inline\"")
    code = _string(req, "code")
    options = _options(req)
    filename, first_line = _location(req, "cell" if mode == "block" else "inline")
    session = _sessions.get(name)
    if session is None:
        raise RequestError(f"position mismatch: session '{name}' does not exist, "
                           f"request expects {_show_position(after)}")
    if session.position != after:
        raise RequestError(f"position mismatch: session '{name}' is at "
                           f"{_show_position(session.position)}, "
                           f"request expects {_show_position(after)}")
    # Unknown until the cell completes: a kernel error leaves it null.
    session.position = None
    if mode == "block":
        failed, outputs = _exec_block(session, code, filename, first_line, options)
    else:
        failed, outputs = _exec_inline(session, code, filename, first_line)
    session.position = cell_id
    return {"ok": True, "failed": failed, "outputs": outputs}


def _op_shutdown(req):
    return {"ok": True}


_HANDLERS = {"hello": _op_hello, "reset": _op_reset, "exec": _op_exec,
             "shutdown": _op_shutdown}


def _handle(raw, token):
    """Answer one request line; return (response, exit after sending)."""
    try:
        req = json.loads(raw.decode("utf-8"))
    except ValueError as exc:  # includes UnicodeDecodeError and JSONDecodeError
        return {"ok": False, "error": f"malformed request: {exc}"}, False
    if not isinstance(req, dict):
        return {"ok": False, "error": "malformed request: not a JSON object"}, False
    given = req.get("token")
    if not isinstance(given, str) or not hmac.compare_digest(given.encode("utf-8"), token):
        return {"ok": False, "error": "invalid token"}, True
    op = req.get("op")
    handler = _HANDLERS.get(op) if isinstance(op, str) else None
    if handler is None:
        return {"ok": False, "error": f"unknown op: {json.dumps(op)}"}, False
    try:
        return handler(req), op == "shutdown"
    except RequestError as exc:
        return {"ok": False, "error": str(exc)}, False
    except Exception as exc:
        _log(f"internal error handling '{op}':\n{traceback.format_exc().rstrip()}")
        return {"ok": False, "error": f"internal error: {type(exc).__name__}: {exc}"}, False


def _send(conn, message):
    text = json.dumps(message, ensure_ascii=False)
    # Lone surrogates (from surrogateescape-decoded text, say) are not
    # encodable as UTF-8.
    text = _SURROGATE.sub("\ufffd", text)
    conn.sendall(text.encode("utf-8") + b"\n")


def _serve(conn, token):
    """Answer requests until loom closes the connection; return an exit status."""
    try:
        with conn.makefile("rb") as reader:
            for raw in reader:
                response, stop = _handle(raw, token)
                _send(conn, response)
                if stop:
                    _log("shutdown requested" if response["ok"] else
                         "request with an invalid token; exiting")
                    return 0 if response["ok"] else 1
    except OSError as exc:
        _log(f"connection error: {exc}; exiting")
        return 1
    _log("connection closed by loom; exiting")
    return 0


# ── Process ─────────────────────────────────────────────────────────────────


def _process_exists(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True  # exists, owned by another user
    return True


def _watch_parent(pid):
    while True:
        time.sleep(PARENT_CHECK_INTERVAL)
        if not _process_exists(pid):
            _log(f"parent process {pid} no longer exists; exiting")
            os._exit(0)


def _parse_args(argv):
    parser = argparse.ArgumentParser(description="Python kernel for loom.")
    parser.add_argument("--connect", type=int, required=True, metavar="PORT")
    parser.add_argument("--token", required=True)
    parser.add_argument("--parent-pid", type=int, required=True)
    parser.add_argument("--scratch", required=True)
    args = parser.parse_args(argv)
    if not 0 < args.connect < 65536:
        parser.error("--connect must be a port between 1 and 65535")
    if args.parent_pid <= 0:
        # os.kill(0, ...) would address the whole process group.
        parser.error("--parent-pid must be a positive process id")
    return args


def _isolate_process():
    """Prepare the interpreter state that user code sees."""
    # input() and child processes must not read the terminal loom runs in.
    devnull = os.open(os.devnull, os.O_RDONLY)
    os.dup2(devnull, 0)
    os.close(devnull)
    sys.stdin = open(0, "r", encoding="utf-8", closefd=False)
    sys.stdout = _OutputRouter("stdout", sys.stdout)
    sys.stderr = _OutputRouter("stderr", sys.stderr)
    warnings.showwarning = _showwarning
    if "matplotlib.pyplot" not in sys.modules:
        sys.meta_path.insert(0, _PyplotImportHook())
    # User code sees an interactive interpreter: no kernel arguments (the
    # token among them) and the document directory, not the kernel's, first
    # on the import path.
    sys.argv = [""]
    kernel_dir = os.path.dirname(os.path.abspath(__file__))
    if sys.path and os.path.abspath(sys.path[0] or os.curdir) == kernel_dir:
        sys.path[0] = ""


def main(argv=None):
    global _scratch
    if os.name != "posix":
        sys.exit("the loom Python kernel requires a POSIX system")
    args = _parse_args(argv)
    if not _process_exists(args.parent_pid):
        _log(f"parent process {args.parent_pid} does not exist; exiting")
        return 1
    _scratch = os.path.abspath(args.scratch)
    os.makedirs(_scratch, exist_ok=True)
    token = args.token.encode("utf-8")

    # Blocking with no timeout: the connection may sit idle for hours.
    conn = socket.create_connection(("127.0.0.1", args.connect), timeout=None)
    _send(conn, {"token": args.token})

    threading.Thread(target=_watch_parent, args=(args.parent_pid,),
                     name="loom-parent-watch", daemon=True).start()
    _isolate_process()
    _log(f"Python {platform.python_version()} connected to 127.0.0.1:{args.connect}")
    status = _serve(conn, token)
    conn.close()
    # os._exit: threads started by user code must not keep the kernel alive.
    _LOG.flush()
    os._exit(status)


if __name__ == "__main__":
    sys.exit(main())
