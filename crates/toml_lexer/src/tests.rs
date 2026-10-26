use expect_test::{Expect, expect};

use crate::{Lexer, TokenKind, parse_float, parse_integer, unquote};

fn check(src: &str, expect: Expect) {
    let mut out = String::new();
    let mut joined = String::new();
    for t in Lexer::new(src) {
        joined.push_str(t.text);
        out.push_str(&format!("{:?} {:?}", t.kind, t.text));
        if let Some(e) = t.error {
            out.push_str(&format!(" error: {e}"));
        }
        out.push('\n');
    }
    assert_eq!(joined, src, "the tokens must cover the input");
    expect.assert_eq(&out);
}

#[test]
fn keys_and_values() {
    check(
        "a.b = 1979-05-27\n1234 = true\n\"q\".'l' = -1.5e3\n",
        expect![[r#"
            BareKey "a"
            Dot "."
            BareKey "b"
            Whitespace " "
            Eq "="
            Whitespace " "
            LocalDate "1979-05-27"
            Newline "\n"
            BareKey "1234"
            Whitespace " "
            Eq "="
            Whitespace " "
            Bool "true"
            Newline "\n"
            BasicString "\"q\""
            Dot "."
            LiteralString "'l'"
            Whitespace " "
            Eq "="
            Whitespace " "
            Float "-1.5e3"
            Newline "\n"
        "#]],
    );
}

#[test]
fn headers_and_brackets() {
    check(
        "[[a.b]]\n[c]\nd = [[1], {e = 2, f = [3]}]\n",
        expect![[r#"
            DoubleLBracket "[["
            BareKey "a"
            Dot "."
            BareKey "b"
            DoubleRBracket "]]"
            Newline "\n"
            LBracket "["
            BareKey "c"
            RBracket "]"
            Newline "\n"
            BareKey "d"
            Whitespace " "
            Eq "="
            Whitespace " "
            LBracket "["
            LBracket "["
            Integer "1"
            RBracket "]"
            Comma ","
            Whitespace " "
            LBrace "{"
            BareKey "e"
            Whitespace " "
            Eq "="
            Whitespace " "
            Integer "2"
            Comma ","
            Whitespace " "
            BareKey "f"
            Whitespace " "
            Eq "="
            Whitespace " "
            LBracket "["
            Integer "3"
            RBracket "]"
            RBrace "}"
            RBracket "]"
            Newline "\n"
        "#]],
    );
}

#[test]
fn date_times() {
    check(
        "a = 1979-05-27 07:32:00.5+09:00 # c\nb = 07:32\nc = 1979-02-30\n",
        expect![[r##"
            BareKey "a"
            Whitespace " "
            Eq "="
            Whitespace " "
            OffsetDateTime "1979-05-27 07:32:00.5+09:00"
            Whitespace " "
            Comment "# c"
            Newline "\n"
            BareKey "b"
            Whitespace " "
            Eq "="
            Whitespace " "
            LocalTime "07:32"
            Newline "\n"
            BareKey "c"
            Whitespace " "
            Eq "="
            Whitespace " "
            LocalDate "1979-02-30" error: invalid date
            Newline "\n"
        "##]],
    );
}

#[test]
fn errors() {
    check(
        "a = 01\nb = 1__0\nc = word\nd = \"open\ne = \"\\q\"\nf% = 1\r\n",
        expect![[r#"
            BareKey "a"
            Whitespace " "
            Eq "="
            Whitespace " "
            Integer "01" error: leading zeros are not allowed
            Newline "\n"
            BareKey "b"
            Whitespace " "
            Eq "="
            Whitespace " "
            Integer "1__0" error: underscores must be surrounded by digits
            Newline "\n"
            BareKey "c"
            Whitespace " "
            Eq "="
            Whitespace " "
            Unknown "word" error: invalid value: strings must be quoted
            Newline "\n"
            BareKey "d"
            Whitespace " "
            Eq "="
            Whitespace " "
            BasicString "\"open" error: unterminated string
            Newline "\n"
            BareKey "e"
            Whitespace " "
            Eq "="
            Whitespace " "
            BasicString "\"\\q\"" error: invalid escape sequence
            Newline "\n"
            Unknown "f%" error: invalid bare key: quote keys with other characters than `A-Za-z0-9_-`
            Whitespace " "
            Eq "="
            Whitespace " "
            Integer "1"
            Newline "\r\n"
        "#]],
    );
}

#[test]
fn multiline_strings() {
    check(
        "a = \"\"\"\nx \\\n  y\"\"\"\"\nb = '''q''''\n",
        expect![[r#"
            BareKey "a"
            Whitespace " "
            Eq "="
            Whitespace " "
            MultilineBasicString "\"\"\"\nx \\\n  y\"\"\"\""
            Newline "\n"
            BareKey "b"
            Whitespace " "
            Eq "="
            Whitespace " "
            MultilineLiteralString "'''q''''"
            Newline "\n"
        "#]],
    );
}

#[test]
fn decoding() {
    assert_eq!(unquote(TokenKind::BasicString, r#""a\tb\u00e9\x41""#), "a\tbéA");
    assert_eq!(unquote(TokenKind::MultilineBasicString, "\"\"\"\nx \\\n  y\"\"\"\""), "x y\"");
    assert_eq!(unquote(TokenKind::MultilineLiteralString, "'''\nq\\n'''"), "q\\n");
    assert_eq!(parse_integer("0xdead_beef"), Some(0xdead_beef));
    assert_eq!(parse_integer("-1_000"), Some(-1000));
    assert_eq!(parse_float("6.626e-34"), Some(6.626e-34));
}
