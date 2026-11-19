"""Python kernel for loom.

Implements version 1 of the loom kernel protocol (kernels/PROTOCOL.md in the
loom repository): the kernel connects to loom on 127.0.0.1, identifies itself
with the shared token, then answers one JSON request per line with one JSON
response per line, executing code in named sessions.

Only the standard library is required.  matplotlib is used when user code has
loaded it; the kernel never imports it on its own.  pyarrow (with pandas or
polars for data frames, numpy for arrays) is imported only by requests that
import or export shared data.

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
import importlib
import importlib.metadata
import inspect
import io
import itertools
import json
import linecache
import math
import platform
import re
import signal
import site
import socket
import sys
import sysconfig
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
# Python 3.9 has no list of standard-library modules; module locations decide.
_STDLIB_NAMES = getattr(sys, "stdlib_module_names", frozenset())
_PENDING_FIGURE = "pending-figure"

_sessions = {}
_scratch = None
_current = None  # the _Cell collecting output, or None between cells
_request_running = False  # a reset or exec request is running code
_in_user_code = False  # inside a `with _USER_CODE` block
_interrupt_pending = False  # SIGINT arrived while kernel code of a request ran
_backend_checked = False
_scratch_numbers = itertools.count(1)
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


class _ShareError(Exception):
    """An import or export that failed; the message is the whole error text."""


class _UserCode:
    """Context manager marking code whose exceptions belong to the user.

    Everything raised inside, including SystemExit and KeyboardInterrupt, is
    re-raised as _UserError; exceptions raised elsewhere are kernel errors.
    An interrupt that arrived while kernel code ran is raised on entry, or on
    exit when the block completed.
    """

    def __enter__(self):
        global _in_user_code, _interrupt_pending
        if _interrupt_pending:
            _interrupt_pending = False
            raise _UserError(KeyboardInterrupt())
        _in_user_code = True
        return self

    def __exit__(self, kind, exc, tb):
        global _in_user_code, _interrupt_pending
        _in_user_code = False
        if exc is None:
            if _interrupt_pending:
                _interrupt_pending = False
                raise _UserError(KeyboardInterrupt())
            return False
        if isinstance(exc, _UserError):
            return False
        raise _UserError(exc) from None


_USER_CODE = _UserCode()
_USER_CODE_FRAMES = (_UserCode.__enter__.__code__, _UserCode.__exit__.__code__)


def _on_sigint(signum, frame):
    """SIGINT interrupts the running request and is ignored otherwise.

    KeyboardInterrupt is raised only inside user-code blocks, where it becomes
    a user error; raised in kernel code (socket reads and writes, JSON,
    bookkeeping) it would abort the kernel.  Elsewhere during a request the
    interrupt is recorded and raised at the next block boundary.
    """
    global _interrupt_pending
    if not _request_running:
        return  # idle, reading a request, or writing a response
    if _in_user_code and (frame is None or frame.f_code not in _USER_CODE_FRAMES):
        raise KeyboardInterrupt
    _interrupt_pending = True


def _begin_request():
    global _request_running, _interrupt_pending
    _interrupt_pending = False
    _request_running = True


def _end_request():
    """Stop accepting interrupts; report whether one is still unhandled."""
    global _request_running, _interrupt_pending
    _request_running = False
    pending, _interrupt_pending = _interrupt_pending, False
    return pending


_INTERRUPTED = "interrupted (KeyboardInterrupt)"


def _is_interrupted_error(output):
    return output["type"] == "error" and output["text"].startswith("interrupted")


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
    if isinstance(exc, _ShareError):
        return str(exc)
    tb = exc.__traceback__
    while tb is not None and tb.tb_frame.f_globals is _KERNEL_GLOBALS:
        tb = tb.tb_next
    if isinstance(exc, KeyboardInterrupt):
        # Raised by _on_sigint, whose frame ends the traceback.
        previous, last = None, tb
        while last is not None and last.tb_next is not None:
            previous, last = last, last.tb_next
        if last is not None and last.tb_frame.f_code is _on_sigint.__code__:
            if previous is None:
                tb = None
            else:
                previous.tb_next = None
        if tb is None:
            return _INTERRUPTED
        return _INTERRUPTED + "\n" + "".join(
            traceback.format_exception(type(exc), exc, tb)).rstrip("\n")
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


def _apply_print_width(width):
    """Characters per line of printed output (the `print-width` option), for
    shutil.get_terminal_size and for numpy and pandas when they are loaded."""
    os.environ["COLUMNS"] = str(width)
    np = sys.modules.get("numpy")
    if np is not None:
        np.set_printoptions(linewidth=width)
    pd = sys.modules.get("pandas")
    if pd is not None:
        pd.set_option("display.width", width)


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


def _scratch_path(extension):
    """A new file name in the scratch directory, unique for the life of the process."""
    return os.path.join(_scratch, f"python-{os.getpid()}-{next(_scratch_numbers)}.{extension}")


def _figure_display(fig, fmt):
    path = _scratch_path(fmt)
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
        path = _scratch_path(fmt)
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
                # Before every statement: numpy or pandas may be imported
                # by the previous one.
                _apply_print_width(self.options["print-width"])
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


# ── State and packages ──────────────────────────────────────────────────────


def _is_user_name(name):
    return isinstance(name, str) and not name.startswith("__")


def _changed_names(before, after):
    """Names created, rebound to a different object, or removed, sorted."""
    changed = {name for name, value in after.items()
               if _is_user_name(name) and (name not in before or before[name] is not value)}
    changed.update(name for name in before if _is_user_name(name) and name not in after)
    return sorted(changed)


def _within(path, directories):
    return any(path == d or path.startswith(d.rstrip(os.sep) + os.sep) for d in directories)


def _library_dirs():
    """Real paths of the standard-library directories and of the site directories."""
    paths = sysconfig.get_paths()
    stdlib = {paths["stdlib"], paths["platstdlib"]}
    sites = {paths["purelib"], paths["platlib"]}
    if hasattr(site, "getsitepackages"):
        sites.update(site.getsitepackages())
    if hasattr(site, "getusersitepackages"):
        sites.add(site.getusersitepackages())
    return ({os.path.realpath(d) for d in stdlib if d},
            {os.path.realpath(d) for d in sites if d})


def _module_location(module):
    """A package's directory or a module's file, or None.

    Reads the module's __dict__ directly: attribute access on a lazily
    loaded module would execute it.
    """
    try:
        attrs = object.__getattribute__(module, "__dict__")
    except AttributeError:
        return None
    file = attrs.get("__file__")
    path = attrs.get("__path__")
    if path is not None:
        if isinstance(file, str):
            return os.path.dirname(os.path.abspath(file))
        entries = list(path)  # namespace package
        return os.path.abspath(entries[0]) if entries and isinstance(entries[0], str) else None
    return os.path.abspath(file) if isinstance(file, str) else None


def _top_level_names(dist):
    """Top-level modules a distribution provides, as importlib.metadata infers them."""
    declared = (dist.read_text("top_level.txt") or "").split()
    if declared:
        return declared
    names = set()
    for file in dist.files or ():
        parts = file.parts
        top = parts[0] if len(parts) > 1 else (inspect.getmodulename(file.name) or file.name)
        if "." not in top:
            names.add(top)
    return sorted(names)


def _record_file(dist):
    """Absolute path of the distribution's RECORD file, or None (egg-info has none)."""
    meta = getattr(dist, "_path", None)
    if meta is not None:
        record = os.path.join(str(meta), "RECORD")
        return os.path.abspath(record) if os.path.isfile(record) else None
    for file in dist.files or ():
        if file.name == "RECORD" and file.parent.name.endswith(".dist-info"):
            return os.path.abspath(str(file.locate()))
    return None


class _PackageIndex:
    """Loaded top-level modules outside the standard library, by distribution.

    Mapping modules to distributions means reading every installed
    distribution's metadata, which takes tenths of a second; the map is
    rebuilt only when a module appears that was not loaded at the last
    build, and each distribution's metadata is read once.
    """

    def __init__(self):
        self._dirs = None
        self._metadata = {}  # metadata directory -> (name, top-level names)
        self._dists = None  # top-level module name -> [(name, Distribution)]
        self._known = frozenset()  # modules loaded when _dists was built
        self._entries = {}  # (module name, location) -> [entry]

    def report(self):
        if self._dirs is None:
            self._dirs = _library_dirs()
        modules = {}
        for name, module in list(sys.modules.items()):
            if ("." in name or module is None or name.startswith("__")
                    or name == _BACKEND_MODULE or name in sys.builtin_module_names
                    or name in _STDLIB_NAMES):
                continue
            modules[name] = module
        if self._dists is None or not self._known.issuperset(modules):
            self._dists = self._map_distributions()
            self._known = frozenset(modules)
            self._entries = {}
        found = {}
        for name, module in modules.items():
            location = _module_location(module)
            entries = self._entries.get((name, location))
            if entries is None:
                entries = self._entries[(name, location)] = self._describe(name, location)
            for entry in entries:
                found[(entry["name"], entry["kind"], entry["path"])] = entry
        return [found[key] for key in sorted(found)]

    def _map_distributions(self):
        mapping = {}
        seen = set()
        for dist in importlib.metadata.distributions():
            meta = getattr(dist, "_path", None)
            cached = self._metadata.get(str(meta)) if meta is not None else None
            if cached is None:
                cached = (dist.metadata["Name"], _top_level_names(dist))
                if meta is not None:
                    self._metadata[str(meta)] = cached
                if not cached[0]:
                    _log(f"ignoring a distribution without a name: {meta}")
            name, tops = cached
            if not name:
                continue
            key = re.sub(r"[-_.]+", "-", name).lower()
            if key in seen:
                continue  # shadowed by the same distribution earlier on sys.path
            seen.add(key)
            for top in tops:
                mapping.setdefault(top, []).append((name, dist))
        return mapping

    def _describe(self, name, location):
        stdlib_dirs, site_dirs = self._dirs
        real = os.path.realpath(location) if location is not None else None
        if real is not None and _within(real, stdlib_dirs) and not _within(real, site_dirs):
            return []  # standard library
        dists = self._dists.get(name)
        if not dists:
            # Without a location there is nothing to report.  Modules under a
            # site directory without a distribution of their own belong to an
            # installed package (Cython's helper modules, for example).
            if real is None or _within(real, site_dirs):
                return []
            return [{"name": name, "version": None, "kind": "tree", "path": location}]
        entries = []
        for dist_name, dist in dists:
            record = _record_file(dist)
            base = os.path.realpath(str(dist.locate_file("")))
            if record is not None and (real is None or _within(real, {base})):
                entries.append({"name": dist_name, "version": dist.version,
                                "kind": "file", "path": record})
            elif location is not None:
                # An editable install: the code lives outside the install location.
                entries.append({"name": dist_name, "version": dist.version,
                                "kind": "tree", "path": location})
        return entries


_PACKAGES = _PackageIndex()


# ── Shared data ─────────────────────────────────────────────────────────────

_SHAREABLE = ("only data frames (pandas or polars DataFrame), numpy arrays with a "
              "numeric, boolean, or string dtype, and scalars (int, float, bool, str, "
              "or a numpy scalar of those kinds) can be shared")
_COLUMN_TYPES = ("data frame columns may hold integers, floats, booleans, strings, "
                 "categoricals, dates, and date-times")


def _share_module(name, label):
    """Import a package needed for sharing data; its absence is a cell error."""
    try:
        return importlib.import_module(name)
    except ImportError as exc:
        raise _ShareError(f"{label}: sharing data needs the Python package '{name}', "
                          f"which cannot be imported ({exc})") from None


def _is_text_type(pa, t):
    return (pa.types.is_string(t) or pa.types.is_large_string(t)
            or (hasattr(pa.types, "is_string_view") and pa.types.is_string_view(t)))


def _type_name(value):
    kind = type(value)
    if kind.__module__ == "builtins":
        return kind.__qualname__
    return f"{kind.__module__}.{kind.__qualname__}"


def _read_shared(name, path, frames):
    """The value stored in the Arrow IPC stream at `path`."""
    label = f"import '{name}'"
    if not name.isidentifier():
        raise _ShareError(f"{label}: not a valid Python name")
    pa = _share_module("pyarrow", label)
    ipc = _share_module("pyarrow.ipc", label)
    try:
        with pa.OSFile(path, "rb") as source:
            table = ipc.open_stream(source).read_all()
    except Exception as exc:
        raise _ShareError(f"{label}: cannot read {path}: {type(exc).__name__}: {exc}") from None
    meta = table.schema.metadata or {}
    kind = meta.get(b"loom.kind", b"").decode("utf-8", "replace")
    try:
        if kind == "dataframe":
            return _read_dataframe(label, table, frames, pa)
        if kind == "array":
            return _read_array(label, table, meta, pa)
        if kind == "scalar":
            return _read_scalar(label, table, pa)
    except _ShareError:
        raise
    except Exception as exc:
        raise _ShareError(f"{label}: cannot convert {path}: {type(exc).__name__}: {exc}") from None
    raise _ShareError(f"{label}: {path} has loom.kind {kind!r}; "
                      "expected 'dataframe', 'array', or 'scalar'")


def _read_dataframe(label, table, frames, pa):
    if frames == "polars":
        return _share_module("polars", label).from_arrow(table)
    pd = _share_module("pandas", label)
    frame = table.to_pandas()
    # pandas turns integer columns with missing values into float64 and
    # boolean ones into object; the nullable dtypes keep the column's type.
    nullable = {pa.int8(): pd.Int8Dtype(), pa.int16(): pd.Int16Dtype(),
                pa.int32(): pd.Int32Dtype(), pa.int64(): pd.Int64Dtype(),
                pa.uint8(): pd.UInt8Dtype(), pa.uint16(): pd.UInt16Dtype(),
                pa.uint32(): pd.UInt32Dtype(), pa.uint64(): pd.UInt64Dtype(),
                pa.bool_(): pd.BooleanDtype()}
    for i, column in enumerate(table.columns):
        if column.null_count and column.type in nullable:
            frame.isetitem(i, column.to_pandas(types_mapper=nullable.get))
    return frame


def _read_array(label, table, meta, pa):
    np = _share_module("numpy", label)
    if table.column_names != ["values"]:
        raise _ShareError(f"{label}: an array has one column named 'values', "
                          f"not {table.column_names}")
    try:
        shape = json.loads(meta[b"loom.shape"])
    except (KeyError, ValueError):
        raise _ShareError(f"{label}: an array needs a JSON list in loom.shape") from None
    if not (isinstance(shape, list)
            and all(isinstance(d, int) and not isinstance(d, bool) and d >= 0 for d in shape)):
        raise _ShareError(f"{label}: loom.shape {shape!r} is not a list of dimensions")
    column = table.column(0)
    if math.prod(shape) != len(column):
        raise _ShareError(f"{label}: loom.shape {shape} does not match {len(column)} values")
    if column.null_count:
        raise _ShareError(f"{label}: the array has missing values, which arrays cannot hold")
    t = column.type
    if pa.types.is_integer(t) or pa.types.is_floating(t) or pa.types.is_boolean(t):
        values = np.array(column.to_numpy())  # a writable copy
    elif _is_text_type(pa, t):
        values = np.array(column.to_pylist(), dtype=str)
    else:
        raise _ShareError(f"{label}: arrays of Arrow type {t} cannot be read")
    # The elements are stored in column-major order.
    return values.reshape(shape, order="F")


def _read_scalar(label, table, pa):
    if table.column_names != ["value"] or table.num_rows != 1:
        raise _ShareError(f"{label}: a scalar has one column named 'value' and one row")
    t = table.schema.field(0).type
    value = table.column(0)[0]
    if not value.is_valid:
        raise _ShareError(f"{label}: the scalar is a missing value")
    if not (pa.types.is_integer(t) or pa.types.is_floating(t) or pa.types.is_boolean(t)
            or _is_text_type(pa, t)):
        raise _ShareError(f"{label}: scalars of Arrow type {t} cannot be read")
    return value.as_py()


def _shared_kind(label, value):
    """Which kind of shareable value `value` is; anything else is an error."""
    pd = sys.modules.get("pandas")
    if pd is not None and isinstance(value, pd.DataFrame):
        return "pandas"
    pl = sys.modules.get("polars")
    if pl is not None and isinstance(value, pl.DataFrame):
        return "polars"
    np = sys.modules.get("numpy")
    if np is not None and isinstance(value, np.ndarray):
        return "array"
    if np is not None and isinstance(value, (np.bool_, np.integer, np.floating, np.str_)):
        return "numpy scalar"
    if type(value) in (bool, int, float, str):
        return "scalar"
    what = "None" if value is None else f"a value of type {_type_name(value)}"
    raise _ShareError(f"{label}: {what} cannot be shared; {_SHAREABLE}")


def _shared_table(label, value):
    """The Arrow table, with loom metadata, that stores `value`."""
    kind = _shared_kind(label, value)
    pa = _share_module("pyarrow", label)
    try:
        if kind == "pandas":
            table = _frame_table(label, pa.Table.from_pandas(value, preserve_index=False), pa)
            meta = {"loom.kind": "dataframe"}
        elif kind == "polars":
            table = _frame_table(label, value.to_arrow(), pa)
            meta = {"loom.kind": "dataframe"}
        elif kind == "array":
            table = pa.table({"values": _array_values(label, value, pa)})
            meta = {"loom.kind": "array",
                    "loom.shape": json.dumps(list(value.shape), separators=(",", ":"))}
        else:
            table = pa.table({"value": _scalar_values(label, value, kind, pa)})
            meta = {"loom.kind": "scalar"}
    except _ShareError:
        raise
    except Exception as exc:
        raise _ShareError(f"{label}: {type(exc).__name__}: {exc}") from None
    # Replacing the metadata also drops pandas' own, so a file reads the same
    # whichever language wrote it.
    return table.replace_schema_metadata(meta)


def _frame_table(label, table, pa):
    """Check a data frame's column types and give text columns one Arrow type."""
    columns = []
    for field, column in zip(table.schema, table.columns):
        t = field.type
        if _is_text_type(pa, t):
            column = column.cast(pa.string())
        elif pa.types.is_dictionary(t):
            if not _is_text_type(pa, t.value_type):
                raise _ShareError(f"{label}: column '{field.name}' is a categorical with "
                                  f"{t.value_type} categories; only text categories can be shared")
            column = column.cast(pa.dictionary(pa.int32(), pa.string(), t.ordered))
        elif pa.types.is_float16(t):
            column = column.cast(pa.float64())
        elif not (pa.types.is_integer(t) or pa.types.is_floating(t) or pa.types.is_boolean(t)
                  or pa.types.is_date(t) or pa.types.is_timestamp(t) or pa.types.is_null(t)):
            raise _ShareError(f"{label}: column '{field.name}' has type {t}, which cannot be "
                              f"shared; {_COLUMN_TYPES}")
        columns.append(column)
    return pa.Table.from_arrays(columns, names=table.column_names)


_INT64_MAX = 2 ** 63 - 1


def _array_values(label, array, pa):
    """The elements of a numpy array in column-major order, as an Arrow array."""
    np = sys.modules["numpy"]
    if isinstance(array, np.ma.MaskedArray):
        if np.ma.is_masked(array):
            raise _ShareError(f"{label}: the array has masked (missing) values, "
                              "which arrays cannot hold")
        array = np.ma.getdata(array)
    dtype = array.dtype
    flat = array.ravel(order="F")
    if dtype.kind == "b":
        return pa.array(flat, type=pa.bool_())
    if dtype.kind in "iu":
        if dtype.itemsize < 4:
            return pa.array(flat.astype(np.int32), type=pa.int32())
        if dtype == np.int32:
            return pa.array(flat, type=pa.int32())
        if dtype.kind == "u" and dtype.itemsize == 8 and flat.size and flat.max() > _INT64_MAX:
            raise _ShareError(f"{label}: a uint64 value exceeds the int64 range")
        return pa.array(flat.astype(np.int64), type=pa.int64())
    if dtype.kind == "f":
        if dtype.itemsize > 8:
            raise _ShareError(f"{label}: {dtype} arrays cannot be shared without losing precision")
        return pa.array(flat.astype(np.float64), type=pa.float64())
    if dtype.kind in "UT":
        values = flat.tolist()
        if any(v is None or not isinstance(v, str) for v in values):
            raise _ShareError(f"{label}: the array has missing values, which arrays cannot hold")
        return pa.array(values, type=pa.string())
    if dtype.kind == "O" and any(v is None for v in flat):
        raise _ShareError(f"{label}: the array contains None; arrays cannot hold missing "
                          "values, and arrays of dtype object cannot be shared")
    raise _ShareError(f"{label}: numpy arrays of dtype {dtype} cannot be shared; {_SHAREABLE}")


def _scalar_values(label, value, kind, pa):
    if kind == "numpy scalar":
        np = sys.modules["numpy"]
        if isinstance(value, np.bool_):
            return pa.array([bool(value)], type=pa.bool_())
        if isinstance(value, np.integer):
            narrow = value.dtype.itemsize < 4 or value.dtype == np.int32
            target = pa.int32() if narrow else pa.int64()
            value = int(value)
            if value > _INT64_MAX:
                raise _ShareError(f"{label}: {value} exceeds the int64 range")
            return pa.array([value], type=target)
        if isinstance(value, np.floating):
            if value.dtype.itemsize > 8:
                raise _ShareError(f"{label}: {value.dtype} cannot be shared without losing precision")
            return pa.array([float(value)], type=pa.float64())
        return pa.array([str(value)], type=pa.string())
    if isinstance(value, bool):
        return pa.array([value], type=pa.bool_())
    if isinstance(value, int):
        if not -_INT64_MAX - 1 <= value <= _INT64_MAX:
            raise _ShareError(f"{label}: {value} exceeds the int64 range")
        return pa.array([value], type=pa.int64())
    if isinstance(value, float):
        return pa.array([value], type=pa.float64())
    return pa.array([value], type=pa.string())


def _remove_files(paths):
    for path in paths:
        if os.path.exists(path):
            os.remove(path)


def _bind_imports(session, imports, frames):
    """Bind every import in the session, or none; return error text or None."""
    try:
        with _USER_CODE:
            values = [(name, _read_shared(name, path, frames)) for name, path in imports]
    except _UserError as err:
        return _format_exception(err.exc)
    for name, value in values:
        session.globals[name] = value
    return None


def _write_exports(session, names):
    """Write each name's value to a new stream file: ([{name, file}], error text or None)."""
    entries = []
    try:
        with _USER_CODE:
            for name in names:
                label = f"export '{name}'"
                if name not in session.globals:
                    raise _ShareError(f"{label}: name '{name}' is not defined")
                table = _shared_table(label, session.globals[name])
                ipc = _share_module("pyarrow.ipc", label)
                path = _scratch_path("arrow")
                entries.append({"name": name, "file": path})
                with ipc.new_stream(path, table.schema) as writer:
                    writer.write_table(table)
    except _UserError as err:
        _remove_files(e["file"] for e in entries)
        return [], _format_exception(err.exc)
    return entries, None


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
    width = options.get("print-width")
    if isinstance(width, bool) or not isinstance(width, int) or width < 20:
        raise _malformed("options 'print-width' must be an integer of at least 20")
    parsed["print-width"] = width
    frames = options.get("dataframe", "pandas")
    if frames not in ("pandas", "polars"):
        raise _malformed("options 'dataframe' must be \"pandas\" or \"polars\"")
    parsed["dataframe"] = frames
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


def _shared_fields(req):
    """The request's imports as (name, file) pairs, and its export names or None."""
    imports = req.get("imports")
    if imports is None:
        imports = []
    if not isinstance(imports, list) or not all(
            isinstance(i, dict) and isinstance(i.get("name"), str)
            and isinstance(i.get("file"), str) for i in imports):
        raise _malformed("'imports' must be a list of {\"name\": ..., \"file\": ...} objects")
    exports = req.get("exports")
    if exports is not None and (not isinstance(exports, list)
                                or not all(isinstance(n, str) for n in exports)):
        raise _malformed("'exports' must be a list of names")
    return [(i["name"], i["file"]) for i in imports], exports


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
    _begin_request()
    try:
        outputs = _run_setup(session, setup)
    finally:
        interrupted = _end_request()
    if interrupted and not outputs:
        outputs = [{"type": "error", "text": _INTERRUPTED}]
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
    imports, exports = _shared_fields(req)
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
    # A shallow copy holds the old objects, so identities cannot be reused.
    before = dict(session.globals)
    exported = []
    _begin_request()
    try:
        error = _bind_imports(session, imports, options["dataframe"]) if imports else None
        if error is not None:
            failed = True
            outputs = [{"type": "error", "text": error}]
            if mode == "block" and code:
                outputs.insert(0, {"type": "source", "code": code})
        elif mode == "block":
            failed, outputs = _exec_block(session, code, filename, first_line, options)
        else:
            failed, outputs = _exec_inline(session, code, filename, first_line)
        if exports and not failed:
            exported, error = _write_exports(session, exports)
            if error is not None:
                failed = True
                if mode == "inline":
                    outputs = []
                outputs.append({"type": "error", "text": error})
    finally:
        interrupted = _end_request()
    if interrupted and not (outputs and _is_interrupted_error(outputs[-1])):
        # Arrived after the last user code ran: the request still fails.
        if mode == "inline":
            outputs = []
        outputs.append({"type": "error", "text": _INTERRUPTED})
        failed = True
    if failed:
        _remove_files(e["file"] for e in exported)
        exported = []
    changed = _changed_names(before, session.globals)
    packages = _PACKAGES.report()
    session.position = cell_id
    response = {"ok": True, "failed": failed, "outputs": outputs,
                "changed": changed, "packages": packages}
    if exports is not None:
        response["exports"] = exported
    return response


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
    signal.signal(signal.SIGINT, _on_sigint)
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
