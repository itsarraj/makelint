//! Structural scan of a Makefile: rule headers (and whether their recipe
//! block ends up non-empty), `.PHONY` declarations, variable
//! assignments, and every variable reference — everything `rules.rs`
//! needs to work from. Kept separate from the rules themselves so the
//! genuinely tricky bits (top-level `:` vs `:=`, `$$` escaping, recipe
//! block boundaries, function calls vs. variable references) each have
//! one obvious place to unit-test in isolation, the same split this
//! workspace's `dockerlint` uses for the same reason.

use std::collections::HashSet;

/// One target header seen in the file, with whether its recipe block
/// (inline via `;` or on the following tab-indented lines) ended up
/// non-empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleTarget {
    pub name: String,
    pub line: usize,
    pub has_recipe: bool,
}

/// A recipe line that should have started with a tab but started with
/// spaces (or some other non-tab character) instead — GNU Make's
/// "missing separator" footgun.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BadIndent {
    pub line: usize,
}

/// A `$(NAME)` / `${NAME}` / bare `ifdef NAME` reference to a variable,
/// with the line it was found on (used to report the *first* use of an
/// undefined one, not every one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarUse {
    pub name: String,
    pub line: usize,
}

#[derive(Debug, Default)]
pub struct Parsed {
    pub targets: Vec<RuleTarget>,
    pub phony: HashSet<String>,
    pub defined_vars: HashSet<String>,
    pub used_vars: Vec<VarUse>,
    pub bad_indents: Vec<BadIndent>,
}

const DIRECTIVE_KEYWORDS: &[&str] = &[
    "ifeq", "ifneq", "ifdef", "ifndef", "else", "endif", "include", "-include", "sinclude",
    "export", "unexport", "vpath", "override", "undefine",
];

/// Characters allowed inside a variable name this scanner will track.
/// GNU Make itself is far more permissive (almost anything but `:`,
/// `#`, `=`, and whitespace), but real-world Makefiles overwhelmingly
/// use `[A-Za-z0-9_.]`, and being stricter here keeps automatic
/// variables (`$@`, `$<`, `$(@D)`, ...) and indirect/computed
/// references (`$($(X))`) from ever being misread as plain names.
fn is_var_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-'
}

/// Finds the first top-level (outside any `$(...)`/`${...}` nesting and
/// before any top-level `#` comment) occurrence of `:` or one of the
/// assignment operators (`:=`, `::=`, `?=`, `+=`, `!=`, `=`).
///
/// This is what tells a rule header apart from a variable assignment —
/// both can contain `:` and `=` *inside* `$(...)` (e.g.
/// `OBJS := $(SRCS:.c=.o)` has a `:` and an `=` inside the parens that
/// must not be mistaken for the line's own operator).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopLevelOp {
    Colon(usize),
    DoubleColon(usize),
    /// (operator_start, operator_end)
    Assign(usize, usize),
}

pub fn find_top_level_op(s: &str) -> Option<TopLevelOp> {
    let bytes = s.as_bytes();
    let mut depth: i32 = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'{' => depth += 1,
            b')' | b'}' => depth -= 1,
            b'#' if depth <= 0 => return None,
            b':' if depth <= 0 => {
                if bytes.get(i + 1) == Some(&b':') {
                    if bytes.get(i + 2) == Some(&b'=') {
                        return Some(TopLevelOp::Assign(i, i + 3));
                    }
                    return Some(TopLevelOp::DoubleColon(i));
                }
                if bytes.get(i + 1) == Some(&b'=') {
                    return Some(TopLevelOp::Assign(i, i + 2));
                }
                return Some(TopLevelOp::Colon(i));
            }
            b'?' | b'+' | b'!' if depth <= 0 && bytes.get(i + 1) == Some(&b'=') => {
                return Some(TopLevelOp::Assign(i, i + 2));
            }
            b'=' if depth <= 0 => {
                return Some(TopLevelOp::Assign(i, i + 1));
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Same top-level-depth idea, applied to finding a `;` that starts an
/// inline recipe on a rule header line (`target: deps ; recipe`).
fn find_top_level_semicolon(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth: i32 = 0;
    for (i, b) in bytes.iter().enumerate() {
        match b {
            b'(' | b'{' => depth += 1,
            b')' | b'}' => depth -= 1,
            b';' if depth <= 0 => return Some(i),
            _ => {}
        }
    }
    None
}

/// Truncates `s` at the first top-level (unparenthesized) `#`, i.e.
/// strips a trailing Makefile comment. Only meaningful for non-recipe
/// lines — a `#` in a recipe line is the shell's business, not make's,
/// and make expands `$(...)` there regardless of what follows it.
fn strip_top_level_comment(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut depth: i32 = 0;
    for (i, b) in bytes.iter().enumerate() {
        match b {
            b'(' | b'{' => depth += 1,
            b')' | b'}' => depth -= 1,
            b'#' if depth <= 0 => return &s[..i],
            _ => {}
        }
    }
    s
}

/// Scans a line's text for `$(NAME)` / `${NAME}` / `$(NAME:pat=repl)`
/// references and records each one, skipping:
/// - `$$` (make's own escape for a literal `$`, e.g. a shell variable
///   written as `$$PATH` inside a recipe — this is never a make
///   variable and must not be flagged),
/// - automatic variables (`$@`, `$<`, `$^`, `$(@D)`, ...), which never
///   start with an identifier character right after the bracket,
/// - function calls (`$(shell ...)`, `$(patsubst ...)`, `$(wildcard
///   ...)`, ...), recognized because the token right after the bracket
///   is followed by whitespace or a comma rather than `)`/`}`/`:`.
///
/// This is a flat single pass, not a real recursive-descent parse: a
/// function call's *arguments* are still scanned for further `$(...)`
/// references as the same left-to-right walk continues over them, so
/// `$(patsubst %.c,%.o,$(SRCS))` correctly records `SRCS` (a variable)
/// and skips `patsubst` (a function) without needing to track bracket
/// nesting at all.
pub fn scan_vars(s: &str, line: usize, out: &mut Vec<VarUse>) {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '$' {
            i += 1;
            continue;
        }
        // `$$` — escaped, literal dollar; consume both and move on.
        if chars.get(i + 1) == Some(&'$') {
            i += 2;
            continue;
        }
        let opener = chars.get(i + 1).copied();
        if opener != Some('(') && opener != Some('{') {
            // Bare `$@`, `$<`, `$*`, etc. — automatic variable, or a
            // trailing/lone `$`. Nothing to record.
            i += 1;
            continue;
        }
        let closer = if opener == Some('(') { ')' } else { '}' };
        let mut j = i + 2;
        let name_start = j;
        while j < chars.len() && is_var_name_char(chars[j]) {
            j += 1;
        }
        let name: String = chars[name_start..j].iter().collect();
        let terminator = chars.get(j).copied();
        let is_plain_ref = matches!(terminator, Some(c) if c == closer || c == ':');
        if is_plain_ref && !name.is_empty() {
            out.push(VarUse { name, line });
        }
        // Whitespace/comma terminator: a function call head (`shell`,
        // `patsubst`, ...) or a malformed/indirect reference — either
        // way, not a variable name to record. Continue the flat scan
        // right after the `$` so anything inside the parens (including
        // nested `$(...)`) still gets its own pass.
        i += 1;
    }
}

fn flush_targets(
    pending: &mut Vec<(String, usize)>,
    has_recipe: &mut bool,
    out: &mut Vec<RuleTarget>,
) {
    for (name, line) in pending.drain(..) {
        // Special (`.PHONY`, `.SUFFIXES`, ...) and pattern (`%.o`)
        // targets are never something a human "forgot" to mark
        // phony — they're not user action targets at all.
        if name.starts_with('.') || name.contains('%') {
            continue;
        }
        out.push(RuleTarget {
            name,
            line,
            has_recipe: *has_recipe,
        });
    }
    *has_recipe = false;
}

/// A physical line ends with an odd number of trailing backslashes,
/// meaning (per Make's own continuation rule) the *next* physical line
/// is a continuation of this logical line, not a new one.
fn ends_with_continuation(line: &str) -> bool {
    line.chars().rev().take_while(|&c| c == '\\').count() % 2 == 1
}

fn is_directive_line(first_word: &str) -> bool {
    DIRECTIVE_KEYWORDS.contains(&first_word)
}

/// Whether an unindented (or space-indented) line encountered while
/// still expecting more recipe lines actually looks like a *new*
/// top-level construct (another rule header, a variable assignment, a
/// directive, a comment) rather than a recipe line that's simply
/// missing its leading tab. Real GNU Make would try exactly this same
/// reparse and, failing it, emit "missing separator".
fn looks_like_new_top_level(trimmed: &str) -> bool {
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return true;
    }
    let first_word = trimmed.split_whitespace().next().unwrap_or("");
    if first_word == "define" || is_directive_line(first_word) {
        return true;
    }
    matches!(
        find_top_level_op(trimmed),
        Some(TopLevelOp::Colon(_))
            | Some(TopLevelOp::DoubleColon(_))
            | Some(TopLevelOp::Assign(_, _))
    )
}

pub fn parse(content: &str) -> Parsed {
    let mut out = Parsed::default();
    let mut in_recipe = false;
    let mut in_define = false;
    let mut pending_targets: Vec<(String, usize)> = Vec::new();
    let mut pending_has_recipe = false;

    let lines: Vec<&str> = content.lines().collect();
    let mut is_continuation_line = false;

    for (idx, raw) in lines.iter().enumerate() {
        let line_no = idx + 1;
        let is_continuation = is_continuation_line;
        is_continuation_line = ends_with_continuation(raw);

        if is_continuation {
            // Continuation lines are folded into the previous logical
            // line by make itself; they don't need their own tab (only
            // the first physical line of a recipe does) and aren't
            // reparsed as their own construct. Skipping them entirely
            // is a deliberate scope cut — see README.
            continue;
        }

        if in_define {
            let t = raw.trim();
            if t == "endef" || t.starts_with("endef") {
                in_define = false;
            }
            continue;
        }

        if in_recipe {
            if raw.trim().is_empty() {
                flush_targets(
                    &mut pending_targets,
                    &mut pending_has_recipe,
                    &mut out.targets,
                );
                in_recipe = false;
                continue;
            }
            if let Some(recipe_body) = raw.strip_prefix('\t') {
                scan_vars(recipe_body, line_no, &mut out.used_vars);
                pending_has_recipe = true;
                continue;
            }
            let trimmed = raw.trim_start();
            if looks_like_new_top_level(trimmed) {
                flush_targets(
                    &mut pending_targets,
                    &mut pending_has_recipe,
                    &mut out.targets,
                );
                in_recipe = false;
                // Fall through: reprocess this same line as top-level below.
            } else {
                out.bad_indents.push(BadIndent { line: line_no });
                flush_targets(
                    &mut pending_targets,
                    &mut pending_has_recipe,
                    &mut out.targets,
                );
                in_recipe = false;
                continue;
            }
        }

        // --- top-level processing ---
        let trimmed_full = raw.trim();
        if trimmed_full.is_empty() || trimmed_full.starts_with('#') {
            continue;
        }
        if raw.starts_with('\t') {
            // A recipe line with no active rule above it (e.g. right at
            // the top of the file). Real make errors on this too
            // ("recipe commences before first target"), but that's a
            // different failure mode than the one this tool targets —
            // ignored quietly rather than misreported as a phony/var
            // issue.
            continue;
        }

        let trimmed = strip_top_level_comment(trimmed_full).trim_end();
        if trimmed.is_empty() {
            continue;
        }
        let first_word = trimmed.split_whitespace().next().unwrap_or("");

        if first_word == "define" {
            let name = trimmed["define".len()..]
                .split_whitespace()
                .next()
                .unwrap_or("");
            if !name.is_empty() {
                out.defined_vars.insert(name.to_string());
            }
            in_define = true;
            continue;
        }

        if is_directive_line(first_word) {
            if first_word == "ifdef" || first_word == "ifndef" {
                if let Some(name) = trimmed.split_whitespace().nth(1) {
                    out.used_vars.push(VarUse {
                        name: name.to_string(),
                        line: line_no,
                    });
                }
            }
            if first_word == "export" || first_word == "unexport" {
                let rest = trimmed[first_word.len()..].trim();
                // `export FOO = bar` both defines and exports; bare
                // `export FOO` (or `export`, exporting everything)
                // just marks an existing/environment name as visible —
                // either way, treat the name as "known" rather than a
                // candidate for an undefined-variable warning.
                if !rest.is_empty() {
                    match find_top_level_op(rest) {
                        Some(TopLevelOp::Assign(start, end)) => {
                            let name = rest[..start].trim();
                            if !name.is_empty() {
                                out.defined_vars.insert(name.to_string());
                            }
                            scan_vars(&rest[end..], line_no, &mut out.used_vars);
                        }
                        _ => {
                            for name in rest.split_whitespace() {
                                out.defined_vars.insert(name.to_string());
                            }
                        }
                    }
                }
            } else {
                scan_vars(trimmed, line_no, &mut out.used_vars);
            }
            continue;
        }

        match find_top_level_op(trimmed) {
            Some(TopLevelOp::Assign(start, end)) => {
                let mut name = trimmed[..start].trim();
                // Defensive handling for a target-specific variable line
                // (`target: VAR = value`) that slips through: fall back
                // to the last whitespace-separated token as the name
                // rather than mis-recording the whole "target: VAR" blob.
                if let Some(last) = name.rsplit(|c: char| c.is_whitespace() || c == ':').next() {
                    if !last.is_empty() {
                        name = last;
                    }
                }
                if !name.is_empty() {
                    out.defined_vars.insert(name.to_string());
                }
                scan_vars(&trimmed[end..], line_no, &mut out.used_vars);
            }
            Some(TopLevelOp::Colon(pos)) => {
                handle_rule_header(
                    trimmed,
                    pos,
                    1,
                    line_no,
                    &mut out,
                    &mut pending_targets,
                    &mut pending_has_recipe,
                    &mut in_recipe,
                );
            }
            Some(TopLevelOp::DoubleColon(pos)) => {
                handle_rule_header(
                    trimmed,
                    pos,
                    2,
                    line_no,
                    &mut out,
                    &mut pending_targets,
                    &mut pending_has_recipe,
                    &mut in_recipe,
                );
            }
            None => {
                // Doesn't parse as anything this tool recognizes.
                // Still worth scanning for variable references
                // defensively rather than silently dropping the line.
                scan_vars(trimmed, line_no, &mut out.used_vars);
            }
        }
    }

    if in_recipe {
        flush_targets(
            &mut pending_targets,
            &mut pending_has_recipe,
            &mut out.targets,
        );
    }

    out
}

#[allow(clippy::too_many_arguments)]
fn handle_rule_header(
    line: &str,
    colon_pos: usize,
    colon_len: usize,
    line_no: usize,
    out: &mut Parsed,
    pending_targets: &mut Vec<(String, usize)>,
    pending_has_recipe: &mut bool,
    in_recipe: &mut bool,
) {
    // A previous header's recipe block should already have been flushed
    // by a blank line or the next construct; flush defensively anyway
    // so two rule headers back-to-back (no recipe on the first) can't
    // bleed targets into each other.
    flush_targets(pending_targets, pending_has_recipe, &mut out.targets);

    let targets_part = line[..colon_pos].trim();
    let rest = &line[colon_pos + colon_len..];
    let names: Vec<String> = targets_part
        .split_whitespace()
        .map(str::to_string)
        .collect();

    if names.len() == 1 && names[0] == ".PHONY" {
        for tok in rest.split_whitespace() {
            out.phony.insert(tok.to_string());
        }
    }

    let has_inline_recipe = if let Some(semi) = find_top_level_semicolon(rest) {
        scan_vars(&rest[..semi], line_no, &mut out.used_vars);
        scan_vars(&rest[semi + 1..], line_no, &mut out.used_vars);
        !rest[semi + 1..].trim().is_empty()
    } else {
        scan_vars(rest, line_no, &mut out.used_vars);
        false
    };

    for name in names {
        pending_targets.push((name, line_no));
    }
    *pending_has_recipe = has_inline_recipe;
    *in_recipe = true;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_top_level_op_finds_a_plain_colon() {
        assert_eq!(find_top_level_op("target: dep"), Some(TopLevelOp::Colon(6)));
    }

    #[test]
    fn find_top_level_op_finds_double_colon() {
        assert_eq!(
            find_top_level_op("target:: dep"),
            Some(TopLevelOp::DoubleColon(6))
        );
    }

    #[test]
    fn find_top_level_op_finds_walrus_assign() {
        assert_eq!(
            find_top_level_op("VAR := value"),
            Some(TopLevelOp::Assign(4, 6))
        );
    }

    #[test]
    fn find_top_level_op_ignores_colon_and_equals_inside_parens() {
        // The `:` and `=` inside $(SRCS:.c=.o) must not be mistaken for
        // the line's own top-level operator.
        assert_eq!(
            find_top_level_op("OBJS := $(SRCS:.c=.o)"),
            Some(TopLevelOp::Assign(5, 7))
        );
    }

    #[test]
    fn find_top_level_op_stops_at_a_top_level_comment() {
        assert_eq!(find_top_level_op("# target: dep"), None);
    }

    #[test]
    fn find_top_level_op_finds_plus_equals() {
        assert_eq!(
            find_top_level_op("VAR += value"),
            Some(TopLevelOp::Assign(4, 6))
        );
    }

    #[test]
    fn scan_vars_records_a_plain_reference() {
        let mut out = Vec::new();
        scan_vars("echo $(NAME)", 1, &mut out);
        assert_eq!(
            out,
            vec![VarUse {
                name: "NAME".to_string(),
                line: 1
            }]
        );
    }

    #[test]
    fn scan_vars_records_brace_form_too() {
        let mut out = Vec::new();
        scan_vars("echo ${NAME}", 1, &mut out);
        assert_eq!(out[0].name, "NAME");
    }

    #[test]
    fn scan_vars_skips_escaped_dollar_dollar() {
        let mut out = Vec::new();
        scan_vars("echo $$PATH", 1, &mut out);
        assert!(out.is_empty(), "$$PATH is a shell variable, not a make one");
    }

    #[test]
    fn scan_vars_skips_automatic_variables() {
        let mut out = Vec::new();
        scan_vars("$(CC) -o $@ $<", 1, &mut out);
        assert_eq!(
            out,
            vec![VarUse {
                name: "CC".to_string(),
                line: 1
            }]
        );
    }

    #[test]
    fn scan_vars_skips_function_calls_but_scans_their_arguments() {
        let mut out = Vec::new();
        scan_vars("$(patsubst %.c,%.o,$(SRCS))", 1, &mut out);
        assert_eq!(
            out,
            vec![VarUse {
                name: "SRCS".to_string(),
                line: 1
            }]
        );
    }

    #[test]
    fn scan_vars_records_substitution_reference_name_only() {
        let mut out = Vec::new();
        scan_vars("$(SRCS:.c=.o)", 1, &mut out);
        assert_eq!(
            out,
            vec![VarUse {
                name: "SRCS".to_string(),
                line: 1
            }]
        );
    }

    #[test]
    fn parse_detects_dot_phony_targets() {
        let parsed = parse(".PHONY: clean test\n");
        assert!(parsed.phony.contains("clean"));
        assert!(parsed.phony.contains("test"));
    }

    #[test]
    fn parse_records_a_target_with_a_tab_indented_recipe() {
        let parsed = parse("build:\n\techo hi\n");
        assert_eq!(parsed.targets.len(), 1);
        assert_eq!(parsed.targets[0].name, "build");
        assert!(parsed.targets[0].has_recipe);
    }

    #[test]
    fn parse_records_a_target_with_no_recipe_at_all() {
        let parsed = parse("all: build test\n");
        assert_eq!(parsed.targets[0].name, "all");
        assert!(!parsed.targets[0].has_recipe);
    }

    #[test]
    fn parse_detects_a_space_indented_recipe_as_bad_indent() {
        let parsed = parse("build:\n    echo hi\n");
        assert_eq!(parsed.bad_indents.len(), 1);
        assert_eq!(parsed.bad_indents[0].line, 2);
    }

    #[test]
    fn parse_pattern_and_special_targets_are_excluded_from_ordinary_targets() {
        let parsed = parse(".SUFFIXES:\n%.o: %.c\n\tgcc -c $<\n");
        assert!(parsed.targets.is_empty());
    }

    #[test]
    fn parse_records_a_simple_variable_assignment() {
        let parsed = parse("VERSION := 1.0.0\n");
        assert!(parsed.defined_vars.contains("VERSION"));
    }

    #[test]
    fn parse_inline_recipe_after_semicolon_counts_as_having_a_recipe() {
        let parsed = parse("build: ; echo hi\n");
        assert!(parsed.targets[0].has_recipe);
    }

    #[test]
    fn parse_empty_inline_recipe_after_semicolon_does_not_count() {
        let parsed = parse("build: ;\n");
        assert!(!parsed.targets[0].has_recipe);
    }

    #[test]
    fn parse_line_continuation_is_not_reparsed_as_its_own_construct() {
        // The continuation line has no tab and no colon, so if it were
        // (wrongly) treated as its own top-level line it would just be
        // silently ignored either way — this asserts the *first* line's
        // recipe is still correctly recognized despite the continuation.
        let parsed = parse("build:\n\techo one \\\n\ttwo\n");
        assert!(parsed.targets[0].has_recipe);
    }

    #[test]
    fn parse_define_block_defines_a_variable_and_its_body_is_not_scanned() {
        let parsed = parse("define GREETING\necho $(UNDEFINED_INSIDE_DEFINE)\nendef\n");
        assert!(parsed.defined_vars.contains("GREETING"));
        assert!(
            parsed.used_vars.is_empty(),
            "define bodies are treated as opaque text, not scanned"
        );
    }

    #[test]
    fn parse_export_with_assignment_both_defines_and_records_uses() {
        let parsed = parse("export FOO := $(BAR)\n");
        assert!(parsed.defined_vars.contains("FOO"));
        assert_eq!(parsed.used_vars[0].name, "BAR");
    }

    #[test]
    fn parse_bare_export_marks_names_as_known_without_a_recipe_context() {
        let parsed = parse("export PATH FOO\n");
        assert!(parsed.defined_vars.contains("FOO"));
    }
}
