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
