// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::fmt::{self, Write};
use std::ops::Deref;

/// Calls [`write!`] with the passed arguments and unwraps the result.
///
/// Useful for writing to things with infallible `Write` implementations like
/// `Source` and `String`.
///
/// [`write!`]: std::write
#[macro_export]
macro_rules! uwrite {
    ($dst:expr, $($arg:tt)*) => {
        indoc::writedoc!($dst, $($arg)*).unwrap()
    };
}

/// Calls [`writeln!`] with the passed arguments and unwraps the result.
///
/// Useful for writing to things with infallible `Write` implementations like
/// `Source` and `String`.
///
/// [`writeln!`]: std::writeln
#[macro_export]
macro_rules! uwriteln {
    ($dst:expr, $($arg:tt)*) => {
        {
            indoc::writedoc!($dst, $($arg)*).unwrap();
            write!($dst, "\n").unwrap()
        }
    };
}

// `#[macro_export]` lands the macros at the crate root; re-export them here too so callers (and the
// re-export in `slint-keyos-platform-build`) can reach them by the `source::` module path they
// have always used.
pub use crate::{uwrite, uwriteln};

pub struct Source {
    s: String,
    indent: usize,
    indent_str: &'static str,
    num_newlines: usize,
}

impl Default for Source {
    fn default() -> Self { Self { s: String::new(), indent: 0, indent_str: "    ", num_newlines: 0 } }
}

impl Source {
    pub fn push_str(&mut self, src: &str) {
        let num_lines = src.lines().count();
        let lines = src.lines();
        for (i, line) in lines.enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with('}') && self.s.ends_with(self.indent_str) {
                for _ in 0..self.indent_str.len() {
                    self.s.pop();
                }
            }
            {
                let s = if num_lines == 1 { line } else { line.trim_start() };
                if !s.is_empty() {
                    self.s.push_str(s);
                    self.num_newlines = 0;
                }
            }
            if trimmed.ends_with('{') {
                self.indent += 1;
            }
            if trimmed.starts_with('}') {
                self.indent = self.indent.saturating_sub(1);
            }
            if i != num_lines - 1 || src.ends_with('\n') {
                self.newline();
            }
        }
    }

    pub fn indent(&mut self, amt: usize) { self.indent += amt; }

    pub fn deindent(&mut self, amt: usize) { self.indent = self.indent.saturating_sub(amt); }

    pub fn newline(&mut self) {
        if self.num_newlines < 2 {
            self.s.push('\n');
            for _ in 0..self.indent {
                self.s.push_str(self.indent_str);
            }

            self.num_newlines += 1;
        }
    }

    pub fn as_mut_string(&mut self) -> &mut String { &mut self.s }
}

impl Write for Source {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.push_str(s);
        Ok(())
    }
}

impl Deref for Source {
    type Target = str;

    fn deref(&self) -> &str { &self.s }
}

impl From<Source> for String {
    fn from(s: Source) -> String { s.s }
}

impl AsRef<str> for Source {
    fn as_ref(&self) -> &str { &self.s }
}

#[cfg(test)]
mod tests {
    use super::Source;

    #[test]
    fn lines() {
        assert_eq!(1, "a\n".lines().count());
    }

    #[test]
    fn simple_append() {
        let mut s = Source::default();
        s.push_str("x");
        assert_eq!(s.s, "x");
        s.push_str("y");
        assert_eq!(s.s, "xy");
        s.push_str("z ");
        assert_eq!(s.s, "xyz ");
        s.push_str(" a ");
        assert_eq!(s.s, "xyz  a ");
        s.push_str("\na");
        assert_eq!(s.s, "xyz  a \na");
    }

    #[test]
    fn newline_remap() {
        let mut s = Source::default();
        s.push_str("function() {\n");
        s.push_str("y\n");
        s.push_str("}\n");
        assert_eq!(s.s, "function() {\n    y\n}\n");
    }

    #[test]
    fn if_else() {
        let mut s = Source::default();
        s.push_str("if () {");
        s.push_str("\n");
        s.push_str("y");
        s.push_str("\n");
        s.push_str("} else if () {");
        s.push_str("\n");
        s.push_str("z");
        s.push_str("\n");
        s.push_str("}");
        s.push_str("\n");
        assert_eq!(s.s, "if () {\n    y\n} else if () {\n    z\n}\n");
    }

    #[test]
    fn trim_ws() {
        let mut s = Source::default();
        s.push_str(
            "function() {
                x
        }",
        );
        assert_eq!(s.s, "function() {\n    x\n}");
    }

    #[test]
    fn many_newlines() {
        let mut s = Source::default();
        s.push_str("x");
        s.push_str("\n");
        s.push_str("\n");
        s.push_str("\n");
        s.push_str("y");

        assert_eq!(s.s, "x\n\ny");
    }

    #[test]
    fn many_newlines_2() {
        let mut s = Source::default();
        s.push_str("x\n\n\ny");
        assert_eq!(s.s, "x\n\ny");
    }

    #[test]
    fn slint_struct_name() {
        use std::fmt::Write;
        let mut s = Source::default();
        let name = "TestOneProps";
        uwriteln!(s, "export struct {name} {{");
        assert_eq!(s.s, "export struct TestOneProps {\n    ");
    }

    #[test]
    fn slint_global() {
        let mut s = Source::default();
        s.push_str(
            "
            export global Navigate {

            // Navigation state
            in property <bool> has-backward;
            in property <bool> has-forward;


            }"
            .trim(),
        );

        assert_eq!(
            s.s,
            "
export global Navigate {
    
    // Navigation state
    in property <bool> has-backward;
    in property <bool> has-forward;
    
}
            "
            .trim()
        );
    }
}
