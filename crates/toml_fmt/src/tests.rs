use expect_test::{Expect, expect};

use crate::{Options, format};

fn check(src: &str, expect: Expect) {
    let out = format(src, &Options::default()).expect("no syntax errors");
    expect.assert_eq(&out);
    // Formatting is idempotent.
    assert_eq!(format(&out, &Options::default()).as_deref(), Some(out.as_str()));
}

#[test]
fn layout() {
    check(
        "\n\n# top\na   =1   # one\n\"b\" . c={x=1,y=[ 1,2 ]}   \n\n\n\n[ t . u ]\nv=[\n  1, # first\n\n  # alone\n  2\n]\n[[ arr ]]\nw = { }\n",
        expect![[r#"
            # top
            a = 1 # one
            "b".c = { x = 1, y = [1, 2] }


            [t.u]
            v = [
              1, # first
              # alone
              2,
            ]
            [[arr]]
            w = {}
        "#]],
    );
}

#[test]
fn syntax_errors_are_not_formatted() {
    assert_eq!(format("a = \n", &Options::default()), None);
}

#[test]
fn alignment() {
    let options = Options { align_entries: true, align_comments: true, ..Options::default() };
    let src = "a = 1 # one\nlong_key = \"x\" # two\n\"日本\" = 3 # three\n\nb = [\n  1,\n]\nc = 2\n[t]\nxy = 1\nz = 2 # z\n";
    let out = format(src, &options).unwrap();
    expect![[r#"
        a        = 1   # one
        long_key = "x" # two
        "日本"   = 3   # three

        b = [
          1,
        ]
        c = 2
        [t]
        xy = 1
        z  = 2 # z
    "#]]
    .assert_eq(&out);
    assert_eq!(format(&out, &options).as_deref(), Some(out.as_str()));
}
