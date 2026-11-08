//! Dependency file parsing.
//!
//! WBuild understands three sources of implicit dependencies:
//!
//! * make-style depfiles produced by GCC/Clang (`-MMD -MF file.d`);
//! * `rustc` dep-info files (also make-style);
//! * MSVC `/showIncludes` output.
//!
//! Parsing them ourselves keeps the build engine free of external tools.

/// Parse a make-style dependency file and return the dependencies of the first
/// rule. Escaped spaces and backslash line continuations are handled.
pub fn parse_make_deps(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;

    // Find the `:` separating target(s) from prerequisites. Skip Windows drive
    // letters such as `C:/...` by requiring the colon not be followed by a
    // path separator.
    let mut colon = None;
    let mut escaped = false;
    while i < chars.len() {
        let c = chars[i];
        if escaped {
            escaped = false;
            i += 1;
            continue;
        }
        match c {
            '\\' => escaped = true,
            ':' => {
                let next = chars.get(i + 1).copied();
                let prev = if i > 0 { Some(chars[i - 1]) } else { None };
                let drive_letter = prev.map(|p| p.is_ascii_alphabetic()).unwrap_or(false)
                    && matches!(next, Some('/' | '\\'));
                if !drive_letter {
                    colon = Some(i);
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }

    let Some(colon) = colon else {
        return Vec::new();
    };

    let mut deps = Vec::new();
    let mut current = String::new();
    let mut escaped = false;
    let mut i = colon + 1;
    while i < chars.len() {
        let c = chars[i];
        if escaped {
            match c {
                '\n' => {
                    // Line continuation: acts as whitespace.
                    flush(&mut current, &mut deps);
                }
                '\r' => {
                    // Part of CRLF after a continuation backslash: consume the
                    // following `\n` and treat the pair as whitespace.
                    if chars.get(i + 1) == Some(&'\n') {
                        i += 1;
                    }
                    flush(&mut current, &mut deps);
                }
                other => current.push(other),
            }
            escaped = false;
            i += 1;
            continue;
        }
        match c {
            '\\' => escaped = true,
            ' ' | '\t' | '\u{0c}' => flush(&mut current, &mut deps),
            '\n' | '\r' => {
                flush(&mut current, &mut deps);
                if !deps.is_empty() {
                    break;
                }
            }
            other => current.push(other),
        }
        i += 1;
    }
    flush(&mut current, &mut deps);

    // Remove empty entries and de-duplicate while preserving order.
    let mut seen = std::collections::HashSet::new();
    deps.retain(|d| !d.is_empty() && seen.insert(d.clone()));
    deps
}

fn flush(current: &mut String, deps: &mut Vec<String>) {
    if !current.is_empty() {
        deps.push(std::mem::take(current));
    }
}

/// Extract include paths from MSVC `/showIncludes` output.
///
/// The prefix is localized by the MSVC toolchain, so several known spellings
/// are recognized. Indentation indicates the include depth; we keep the path.
pub fn parse_msvc_includes(text: &str) -> Vec<String> {
    let mut deps = Vec::new();
    for line in text.lines() {
        if let Some(path) = msvc_include_path(line) {
            deps.push(path);
        }
    }
    let mut seen = std::collections::HashSet::new();
    deps.retain(|d| !d.is_empty() && seen.insert(d.clone()));
    deps
}

fn msvc_include_path(line: &str) -> Option<String> {
    // Known localizations of "Note: including file:".
    const PREFIXES: &[&str] = &[
        "Note: including file:",
        "注意: 包含文件:",
        "注意: 包含文件：",
        "Hinweis: Einlesen der Datei:",
        "Remarque : inclusion du fichier :",
        "Nota: inclusión del archivo:",
        "注: インクルード ファイル:",
        "참고: 포함 파일:",
    ];
    let trimmed = line.trim_start();
    for prefix in PREFIXES {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return Some(rest.trim().to_string());
        }
    }
    None
}

/// Remove `/showIncludes` noise from compiler diagnostics.
pub fn strip_msvc_includes(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        if msvc_include_path(line).is_none() {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_depfile() {
        let text = "build/obj/main.o: src/main.c src/util.h\n";
        assert_eq!(parse_make_deps(text), vec!["src/main.c", "src/util.h"]);
    }

    #[test]
    fn parses_continuations_and_escaped_spaces() {
        let text = "out.o: src/a.c \\\n src/my\\ file.h \\\n  src/b.h\n";
        assert_eq!(
            parse_make_deps(text),
            vec!["src/a.c", "src/my file.h", "src/b.h"]
        );
    }

    #[test]
    fn parses_crlf_and_absolute_windows_paths() {
        let text = "build/obj/x.o: C:/proj/src/x.c \\\r\n C:/proj/inc/x.h\r\n";
        assert_eq!(
            parse_make_deps(text),
            vec!["C:/proj/src/x.c", "C:/proj/inc/x.h"]
        );
    }

    #[test]
    fn handles_mp_phony_rules_and_dedup() {
        let text = "a.o: a.c a.h a.c\n\na.h:\n";
        assert_eq!(parse_make_deps(text), vec!["a.c", "a.h"]);
    }

    #[test]
    fn parses_msvc_includes() {
        let text = "main.c\nNote: including file: C:\\proj\\inc\\a.h\n\
                    注意: 包含文件:   C:\\proj\\inc\\b.h\n";
        assert_eq!(
            parse_msvc_includes(text),
            vec!["C:\\proj\\inc\\a.h", "C:\\proj\\inc\\b.h"]
        );
        assert!(!strip_msvc_includes(text).contains("including file"));
    }
}
