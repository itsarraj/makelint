# makelint

Lints Makefiles for the handful of mistakes that actually bite in
practice: a missing `.PHONY` declaration, a recipe line indented with
spaces instead of a tab (GNU Make's single most notorious footgun), and
a variable that's referenced but never defined anywhere in the file.

## Usage

```bash
makelint              # lints ./Makefile by default
makelint path/to/Makefile
```

Exit code `1` if anything's found, `0` if the file is clean.

## The three rules

- **Missing `.PHONY`**: a target with a real recipe whose name is one of
  a curated list of conventionally-non-file targets (`all`, `clean`,
  `test`, `build`, `install`, `deploy`, `lint`, `fmt`, ... — see
  `rules.rs`) but isn't declared under `.PHONY`. If a file with that
  exact name ever exists in the directory (a `build` binary, a `test`
  fixture folder), Make sees it as "already up to date" and the rule
  silently stops running — a real, well-documented Make gotcha.
- **Tab vs. spaces**: a recipe line indented with spaces instead of a
  literal tab. GNU Make requires the tab; anything else gets you
  `Makefile:N: *** missing separator. Stop.` — this catches it before
  Make does, with the actual offending line number.
- **Undefined variables**: a `$(NAME)`/`${NAME}` reference to a variable
  that's never assigned anywhere in the file and isn't one of Make's own
  built-ins (`CC`, `CFLAGS`, `MAKE`, ...) or a common environment
  variable (`PATH`, `HOME`). Make itself doesn't error on this — it
  silently expands to an empty string, which is exactly the kind of
  silent-wrong-behavior this exists to catch before it ships.

## Status: built and verified, including a real syntax error fixed before it could even compile

- **35 unit tests** (`cargo test --lib`) across two modules:
  - `parser` (24): the trickiest part of this tool by far — telling a
    rule header apart from a variable assignment when both can contain
    `:`/`=` *inside* `$(...)` (`OBJS := $(SRCS:.c=.o)` must not have its
    internal `:`/`=` mistaken for the line's own operator), `$$`-escaped
    literal dollars correctly skipped, automatic variables (`$@`, `$<`)
    correctly not recorded, a function call's *arguments* still scanned
    for real variable references while the function name itself isn't
    (`$(patsubst %.c,%.o,$(SRCS))` records only `SRCS`), line
    continuations not reparsed as their own construct, and `define`
    block bodies treated as opaque text rather than scanned for
    variables (Make itself doesn't expand a `define` body until it's
    actually invoked, so this isn't a scope cut — it's the correct
    behavior).
  - `rules` (11): each of the three rules in isolation (including that a
    target with *no* recipe at all is correctly never flagged for a
    missing `.PHONY`, since that's not the failure mode the rule
    targets), an already-declared-phony target correctly not flagged, a
    known builtin correctly not flagged, a variable used twice only
    reported once, and a combined fixture exercising all three rules on
    different lines at once, sorted correctly by line number.
- **A real syntax error caught before this crate could even compile**:
  `src/parser.rs` had a stray extra `)` — `if (first_word == "ifdef" ||
  first_word == "ifndef") ) {` — left over from an interrupted edit.
  Fixed. The crate had never successfully built until this pass, so
  nothing in it had actually been run before now, including the
  extensive parser logic already written.
- **Live-verified against two realistic Makefiles and the actual
  compiled binary**: a deliberately messy one (three phony-looking
  targets missing `.PHONY`, one space-indented recipe line, one
  undefined `$(SERVICE_NAME)` reference) produced exactly 5 findings
  with correct line numbers and exit code `1`; fixing every one of them
  (adding the `.PHONY` line, fixing the indent, defining
  `SERVICE_NAME`) produced a clean "no issues found" report with exit
  code `0` — confirming there's no leftover false positive once the real
  problems are actually fixed, not just that problems are found.

**Not done / deliberately deferred**: `include`d files aren't followed
(a variable defined in an included `.mk` file looks undefined here —
this only ever sees one file's own text); `ifdef`/`ifndef`-conditional
variable definitions aren't tracked as "defined" inside their branch
(only bare top-level assignments and `define` blocks are); and the
missing-`.PHONY` rule's target-name list is a curated, incomplete
heuristic by necessity — there's no way to know from static text alone
whether an arbitrary target name corresponds to a real file Make should
actually track.
