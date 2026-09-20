//! The three lint rules this tool checks, built on top of `parser::Parsed`.

use crate::parser::Parsed;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub line: usize,
    pub message: String,
}

/// Conventional target names that almost never correspond to a real file
/// on disk — the exact case a missing `.PHONY` declaration causes real
/// breakage for: if a file named `clean` or `test` ever exists in the
/// directory, `make clean`/`make test` silently does nothing (make sees
/// the file is already "up to date"). Not exhaustive — nothing can be,
/// since any name could legitimately be phony — but this is the same
/// pragmatic, curated-list approach every real Makefile linter takes
/// (there's no way to know a target doesn't produce a file without
/// actually running the recipe).
const CONVENTIONALLY_PHONY: &[&str] = &[
    "all",
    "clean",
    "test",
    "build",
    "install",
    "uninstall",
    "run",
    "fmt",
    "lint",
    "check",
    "deploy",
    "docs",
    "help",
    "dev",
    "watch",
    "release",
    "publish",
    "format",
    "default",
];

/// Make's own built-in variables, plus common shell/environment
/// variables a Makefile legitimately references without ever defining
/// itself — flagging these as "undefined" would be noise, not signal.
const KNOWN_BUILTINS: &[&str] = &[
    "CC",
    "CXX",
    "CFLAGS",
    "CXXFLAGS",
    "CPPFLAGS",
    "LDFLAGS",
    "LDLIBS",
    "ARFLAGS",
    "AR",
    "AS",
    "LEX",
    "YACC",
    "RM",
    "MAKE",
    "MAKEFLAGS",
    "MAKEFILE_LIST",
    "MAKECMDGOALS",
    "SHELL",
    "PATH",
    "HOME",
    "USER",
    "PWD",
    "GOPATH",
    "CARGO_HOME",
];

pub fn missing_phony(parsed: &Parsed) -> Vec<Finding> {
    parsed
        .targets
        .iter()
        .filter(|t| t.has_recipe && CONVENTIONALLY_PHONY.contains(&t.name.as_str()) && !parsed.phony.contains(&t.name))
        .map(|t| Finding {
            line: t.line,
            message: format!(
                "target {:?} looks conventionally phony but isn't declared under .PHONY — if a file with that name ever exists, this rule silently stops running",
                t.name
            ),
        })
        .collect()
}

pub fn bad_indents(parsed: &Parsed) -> Vec<Finding> {
    parsed
        .bad_indents
        .iter()
        .map(|b| Finding {
            line: b.line,
            message: "recipe line indented with spaces instead of a tab — GNU Make requires a literal tab here and will fail with \"missing separator\"".to_string(),
        })
        .collect()
}

pub fn undefined_vars(parsed: &Parsed) -> Vec<Finding> {
    let mut seen = std::collections::HashSet::new();
    parsed
        .used_vars
        .iter()
        .filter(|v| {
            !parsed.defined_vars.contains(&v.name)
                && !KNOWN_BUILTINS.contains(&v.name.as_str())
                && seen.insert(v.name.clone())
        })
        .map(|v| Finding {
            line: v.line,
            message: format!(
                "${{{}}} is referenced but never defined in this file (and isn't a known Make builtin) — it'll silently expand to an empty string",
                v.name
            ),
        })
        .collect()
}

pub fn lint(parsed: &Parsed) -> Vec<Finding> {
    let mut findings = Vec::new();
    findings.extend(missing_phony(parsed));
    findings.extend(bad_indents(parsed));
    findings.extend(undefined_vars(parsed));
    findings.sort_by_key(|f| f.line);
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    #[test]
    fn flags_a_conventionally_phony_target_missing_from_dot_phony() {
        let parsed = parse("test:\n\tcargo test\n");
        let findings = missing_phony(&parsed);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("\"test\""));
    }

    #[test]
    fn does_not_flag_a_target_already_declared_phony() {
        let parsed = parse(".PHONY: test\ntest:\n\tcargo test\n");
        assert!(missing_phony(&parsed).is_empty());
    }

    #[test]
    fn does_not_flag_a_target_with_no_recipe() {
        // A dependency-only target (e.g. a grouping alias) isn't the
        // "make clean does nothing because a file called clean exists"
        // failure mode this rule targets.
        let parsed = parse("all: build test\n");
        assert!(missing_phony(&parsed).is_empty());
    }

    #[test]
    fn does_not_flag_a_target_with_an_uncommon_name() {
        let parsed = parse("myapp.bin:\n\tgcc -o myapp.bin main.c\n");
        assert!(missing_phony(&parsed).is_empty());
    }

    #[test]
    fn reports_every_bad_indent_from_the_parser() {
        let parsed = parse("build:\n    echo hi\n");
        let findings = bad_indents(&parsed);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].line, 2);
    }

    #[test]
    fn flags_an_undefined_variable_reference() {
        let parsed = parse("build:\n\techo $(VERSION)\n");
        let findings = undefined_vars(&parsed);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("VERSION"));
    }

    #[test]
    fn does_not_flag_a_defined_variable() {
        let parsed = parse("VERSION := 1.0.0\nbuild:\n\techo $(VERSION)\n");
        assert!(undefined_vars(&parsed).is_empty());
    }

    #[test]
    fn does_not_flag_a_known_builtin() {
        let parsed = parse("build:\n\t$(CC) $(CFLAGS) -o out main.c\n");
        assert!(undefined_vars(&parsed).is_empty());
    }

    #[test]
    fn an_undefined_variable_used_twice_is_only_reported_once() {
        let parsed = parse("build:\n\techo $(FOO)\n\techo $(FOO)\n");
        assert_eq!(undefined_vars(&parsed).len(), 1);
    }

    #[test]
    fn lint_merges_and_sorts_all_three_rule_categories_by_line() {
        // A badly-indented recipe line means make itself never considers
        // that target to genuinely have a working recipe (see the
        // dedicated bad-indent test above), so "build" here deliberately
        // ISN'T also expected to trip missing-phony — this fixture keeps
        // each rule's trigger on its own separate, unambiguous target.
        let content = "test:\n\techo hi\nbuild:\n    echo bye\nrun:\n\techo $(UNDEF)\n";
        let parsed = parse(content);
        let findings = lint(&parsed);

        assert_eq!(findings.len(), 4, "{findings:?}");
        assert!(
            findings.windows(2).all(|w| w[0].line <= w[1].line),
            "not sorted: {findings:?}"
        );
        assert!(findings.iter().any(|f| f.message.contains("\"test\"")));
        assert!(findings.iter().any(|f| f.message.contains("\"run\"")));
        assert!(findings.iter().any(|f| f.message.contains("tab")));
        assert!(findings.iter().any(|f| f.message.contains("UNDEF")));
    }

    #[test]
    fn a_clean_makefile_produces_no_findings() {
        let content = ".PHONY: all\nVERSION := 1.0.0\nall:\n\techo $(VERSION)\n";
        let parsed = parse(content);
        assert!(lint(&parsed).is_empty());
    }
}
