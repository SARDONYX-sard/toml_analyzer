use expect_test::{Expect, expect};

use crate::parse;

fn check(src: &str, expect: Expect) {
    let parse = parse(src);
    assert_eq!(parse.syntax_node().text(), src, "the tree must cover the input");
    expect.assert_eq(&parse.debug_dump());
}

#[test]
fn document() {
    check(
        "a = 1 # c\n[t]\nb.c = [1, {d = 2}]\n[[x]]\n",
        expect![[r##"
            ROOT@0..39
              ENTRY@0..5
                KEY@0..1
                  BARE_KEY@0..1 "a"
                WHITESPACE@1..2 " "
                EQ@2..3 "="
                WHITESPACE@3..4 " "
                INTEGER@4..5 "1"
              WHITESPACE@5..6 " "
              COMMENT@6..9 "# c"
              NEWLINE@9..10 "\n"
              TABLE@10..33
                TABLE_HEADER@10..13
                  L_BRACK@10..11 "["
                  KEY@11..12
                    BARE_KEY@11..12 "t"
                  R_BRACK@12..13 "]"
                NEWLINE@13..14 "\n"
                ENTRY@14..32
                  KEY@14..17
                    BARE_KEY@14..15 "b"
                    DOT@15..16 "."
                    BARE_KEY@16..17 "c"
                  WHITESPACE@17..18 " "
                  EQ@18..19 "="
                  WHITESPACE@19..20 " "
                  ARRAY@20..32
                    L_BRACK@20..21 "["
                    INTEGER@21..22 "1"
                    COMMA@22..23 ","
                    WHITESPACE@23..24 " "
                    INLINE_TABLE@24..31
                      L_BRACE@24..25 "{"
                      ENTRY@25..30
                        KEY@25..26
                          BARE_KEY@25..26 "d"
                        WHITESPACE@26..27 " "
                        EQ@27..28 "="
                        WHITESPACE@28..29 " "
                        INTEGER@29..30 "2"
                      R_BRACE@30..31 "}"
                    R_BRACK@31..32 "]"
                NEWLINE@32..33 "\n"
              ARRAY_TABLE@33..39
                ARRAY_TABLE_HEADER@33..38
                  L_BRACK2@33..35 "[["
                  KEY@35..36
                    BARE_KEY@35..36 "x"
                  R_BRACK2@36..38 "]]"
                NEWLINE@38..39 "\n"
        "##]],
    );
}

#[test]
fn recovery() {
    check(
        "a = \nb 1\n[t\nc = [1 2\nd = 3\n",
        expect![[r#"
            ROOT@0..27
              ENTRY@0..3
                KEY@0..1
                  BARE_KEY@0..1 "a"
                WHITESPACE@1..2 " "
                EQ@2..3 "="
              WHITESPACE@3..4 " "
              NEWLINE@4..5 "\n"
              ENTRY@5..8
                KEY@5..6
                  BARE_KEY@5..6 "b"
                WHITESPACE@6..7 " "
                BARE_KEY@7..8 "1"
              NEWLINE@8..9 "\n"
              TABLE@9..27
                TABLE_HEADER@9..11
                  L_BRACK@9..10 "["
                  KEY@10..11
                    BARE_KEY@10..11 "t"
                NEWLINE@11..12 "\n"
                ENTRY@12..21
                  KEY@12..13
                    BARE_KEY@12..13 "c"
                  WHITESPACE@13..14 " "
                  EQ@14..15 "="
                  WHITESPACE@15..16 " "
                  ARRAY@16..21
                    L_BRACK@16..17 "["
                    INTEGER@17..18 "1"
                    WHITESPACE@18..19 " "
                    INTEGER@19..20 "2"
                    NEWLINE@20..21 "\n"
                ENTRY@21..26
                  KEY@21..22
                    UNKNOWN@21..22 "d"
                  WHITESPACE@22..23 " "
                  EQ@23..24 "="
                  WHITESPACE@24..25 " "
                  INTEGER@25..26 "3"
                NEWLINE@26..27 "\n"
            error 3..3: expected a value
            error 6..6: expected `=`, found `1`
            error 11..11: expected `]`
            error 18..18: expected `,` or `]`, found `2`
            error 21..22: invalid value: strings must be quoted
            error 21..22: expected `]`, found `d`
        "#]],
    );
}
