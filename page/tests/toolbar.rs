//! The toolbar's line section, read straight from `web/index.html`: thickness, arrowheads and
//! line style are three buttons side by side, each opening its own menu, and new arrows start
//! with a head at the end.

use wb_page::shape::{Dash, Heads};

const HTML: &str = include_str!("../../web/index.html");

/// The text of the element that opens with `open` (up to its matching `close`).
fn block<'a>(open: &str, close: &str) -> &'a str {
    let i = HTML.find(open).unwrap_or_else(|| panic!("{open} is missing"));
    let j = HTML[i..].find(close).unwrap_or_else(|| panic!("{open} has no {close}"));
    &HTML[i..i + j + close.len()]
}

fn menu(id: &str) -> &'static str {
    block(&format!("<div id=\"{id}\""), "</div>")
}

/// Each `<button ...>...</button>` in `s`.
fn buttons(s: &str) -> Vec<&str> {
    s.match_indices("<button").map(|(i, _)| &s[i..i + s[i..].find("</button>").unwrap() + 9]).collect()
}

fn attr<'a>(b: &'a str, name: &str) -> Option<&'a str> {
    let k = format!(" {name}=\"");
    let i = b.find(&k)? + k.len();
    Some(&b[i..i + b[i..].find('"')?])
}

fn label(b: &str) -> &str {
    let i = b.find("<span>").unwrap() + 6;
    &b[i..i + b[i..].find("</span>").unwrap()]
}

fn svg(b: &str) -> &str {
    let i = b.find("<svg").unwrap();
    &b[i..i + b[i..].find("</svg>").unwrap() + 6]
}

#[test]
fn line_section_is_three_menu_buttons_side_by_side() {
    let bar = block("<div id=\"bar\"", "</div>");
    let ids: Vec<_> = buttons(bar).iter().filter_map(|b| attr(b, "id")).collect();
    let at = ids.iter().position(|&i| i == "widthBtn").expect("widthBtn in the bar");
    assert_eq!(&ids[at..at + 3], ["widthBtn", "headBtn", "dashBtn"]);
    for (btn, m) in [("widthBtn", "widthMenu"), ("headBtn", "headMenu"), ("dashBtn", "dashMenu")] {
        let b = buttons(bar).into_iter().find(|b| attr(b, "id") == Some(btn)).unwrap();
        assert_eq!(attr(b, "aria-haspopup"), Some("menu"), "{btn}");
        assert_eq!(attr(b, "aria-controls"), Some(m), "{btn}");
        assert_eq!(attr(b, "class"), Some("tools"), "{btn} keeps the toolbar style");
    }
    // the old S/M/L buttons are gone: thickness lives only in its menu
    assert!(!bar.contains("data-w="), "no thickness buttons left in the bar");
}

#[test]
fn thickness_menu_is_small_medium_large_each_drawn_at_its_width() {
    let items = buttons(menu("widthMenu"));
    let got: Vec<_> = items.iter().map(|b| (attr(b, "data-w").unwrap(), label(b))).collect();
    assert_eq!(got, [("0.002", "Small"), ("0.004", "Medium"), ("0.009", "Large")]);
    let sw: Vec<f64> = items.iter().map(|b| attr(svg(b), "stroke-width").unwrap().parse().unwrap()).collect();
    assert!(sw.windows(2).all(|w| w[0] < w[1]), "items get thicker: {sw:?}");
    assert!(items.iter().all(|b| attr(b, "role") == Some("menuitemradio")));
    // Medium is the default pen and is the one marked before the page script runs
    let on: Vec<_> = items.iter().filter(|b| attr(b, "aria-checked") == Some("true")).map(|b| label(b)).collect();
    assert_eq!(on, ["Medium"]);
}

#[test]
fn thickness_icon_is_three_lines_thin_to_thick() {
    let b = block("<button type=\"button\" id=\"widthBtn\"", "</button>");
    let sw: Vec<f64> = svg(b).match_indices("<path").map(|(i, _)| attr(&svg(b)[i..], "stroke-width").unwrap().parse().unwrap()).collect();
    assert_eq!(sw.len(), 3);
    assert!(sw.windows(2).all(|w| w[0] < w[1]), "{sw:?}");
}

#[test]
fn arrowheads_default_to_end_and_the_icon_starts_on_it() {
    assert_eq!(Heads::default(), Heads::End);
    let items = buttons(menu("headMenu"));
    let keys: Vec<_> = items.iter().map(|b| (attr(b, "data-head").unwrap(), label(b))).collect();
    assert_eq!(keys, [("none", "None"), ("end", "End"), ("start", "Start"), ("both", "Both")]);
    assert!(keys.iter().all(|(k, _)| Heads::from_key(k).is_some()));
    let end = items.iter().find(|b| attr(b, "data-head") == Some(Heads::default().key())).unwrap();
    let btn = block("<button type=\"button\" id=\"headBtn\"", "</button>");
    assert_eq!(svg(btn), svg(end), "before the page script runs, the button shows End (a chevron)");
}

#[test]
fn line_style_menu_and_its_icon_show_solid_dashed_dotted() {
    let items = buttons(menu("dashMenu"));
    let got: Vec<_> = items.iter().map(|b| label(b)).collect();
    assert_eq!(got, ["Solid", "Dashed", "Dotted"]);
    for b in &items {
        let k = attr(b, "data-dash").unwrap();
        assert_eq!(Dash::from_menu_key(k).map(|d| d.menu_key()), Some(k), "{k} round-trips");
    }
    let btn = svg(block("<button type=\"button\" id=\"dashBtn\"", "</button>"));
    let dashes: Vec<_> = btn.match_indices("<path").map(|(i, _)| attr(&btn[i..i + btn[i..].find("/>").unwrap()], "stroke-dasharray")).collect();
    assert_eq!(dashes, [None, Some("4 4"), Some("0 4")]);
}
