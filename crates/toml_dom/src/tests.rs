use expect_test::{Expect, expect};

use crate::Dom;

fn check(src: &str, expect: Expect) {
    let parse = toml_parser::parse(src);
    assert!(parse.errors().is_empty(), "syntax errors: {:?}", parse.errors());
    let dom = Dom::build(&parse);
    let mut out = serde_json::to_string(&dom.to_json()).unwrap();
    out.push('\n');
    for e in &dom.errors {
        out.push_str(&format!("{:?} {}", e.range, e.message));
        if let Some((r, m)) = &e.related {
            out.push_str(&format!(" ({m} at {r:?})"));
        }
        out.push('\n');
    }
    expect.assert_eq(&out);
}

#[test]
fn valid() {
    check(
        r#"
a.b = 1
"q k" = 1979-05-27
[t.u]
v = [1, {w = true}]
[t]
x = 1.5
[[arr]]
y = 1
[[arr]]
y = 2
[arr.sub]
z = "s"
[fruit]
apple.color = "red"
[fruit.apple.texture]
smooth = true
"#,
        expect![[r#"
            {"a":{"b":1},"arr":[{"y":1},{"sub":{"z":"s"},"y":2}],"fruit":{"apple":{"color":"red","texture":{"smooth":true}}},"q k":"1979-05-27","t":{"u":{"v":[1,{"w":true}]},"x":1.5}}
        "#]],
    );
}

#[test]
fn invalid() {
    check(
        r#"
a = 1
a = 2
b.c = 1
[b]
[t]
[t]
[i]
x = {y = 1}
x.z = 2
[s]
arr = [1]
[[s.arr]]
[h.k]
[h]
k.l = 1
"#,
        expect![[r#"
            {"a":1,"b":{"c":1},"h":{"k":{}},"i":{"x":{"y":1}},"s":{"arr":[1]},"t":{}}
            7..8 the key `a` is defined twice (first defined here at 1..2)
            22..23 the table `b` is already defined by dotted keys (first defined here at 13..14)
            30..31 the table `t` is defined twice (first defined here at 26..27)
            49..50 cannot add keys to the inline table `x` (defined here at 37..38)
            75..78 cannot append to the static array `s.arr` (defined here at 61..64)
            91..92 the table `k` is defined by a header and cannot be extended with dotted keys (defined here at 84..85)
        "#]],
    );
}
