use serde_json::json;

use crate::fixture::{TestServer, evaluate, ok, open_page, server};

const PAGE: &[u8] = b"<!doctype html><meta charset=utf-8><body></body>";

// https://html.spec.whatwg.org/multipage/input.html#the-value-attribute
// https://html.spec.whatwg.org/multipage/input.html#the-input-element
const SANITIZE_FRAGMENT: &str = r#"(() => {
  const host = document.createElement('div');
  host.innerHTML = '<input id=n type=number value=abc>' +
    '<input id=r type=range>' +
    '<input id=c type=color>' +
    '<input id=d type=date value=nope>' +
    '<input id=t type=text value="a\nb">';
  document.body.append(host);
  const template = document.createElement('template');
  template.innerHTML = '<input id=tn type=number value=abc>';
  const xml = new DOMParser().parseFromString(
    '<root><input xmlns="http://www.w3.org/1999/xhtml" type="number" value="abc"/></root>',
    'application/xml',
  );
  return [
    host.querySelector('#n').value,
    host.querySelector('#r').value,
    host.querySelector('#c').value,
    host.querySelector('#d').value,
    host.querySelector('#t').value,
    template.content.querySelector('#tn').value,
    xml.querySelector('input').value,
  ];
})()"#;

// https://html.spec.whatwg.org/multipage/form-elements.html#the-option-element
const SELECT_EARLIER_OPTION: &str = "(() => {
  const select = document.createElement('select');
  select.innerHTML = '<option id=a>A</option><option id=b selected>B</option>';
  document.body.append(select);
  const first = select.querySelector('#a');
  first.setAttribute('selected', '');
  return [select.selectedIndex, first.selected, select.querySelector('#b').selected];
})()";

// https://html.spec.whatwg.org/multipage/input.html#concept-input-checked-dirty
// https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#concept-form-reset-control
const ACTIVATION_AND_RESET: &str = "(() => {
  const form = document.createElement('form');
  form.innerHTML = '<input id=c type=checkbox checked>';
  document.body.append(form);
  const checkbox = form.querySelector('#c');
  checkbox.click();
  const afterClick = checkbox.checked;
  checkbox.removeAttribute('checked');
  checkbox.setAttribute('checked', '');
  const afterAttribute = checkbox.checked;
  form.reset();
  return [afterClick, afterAttribute, checkbox.checked];
})()";

// https://html.spec.whatwg.org/multipage/input.html#concept-input-checked
const CHECKED_BEFORE_LAYOUT: &str = "(() => {
  const box = document.createElement('input');
  box.type = 'checkbox';
  box.checked = true;
  document.body.append(box);
  void box.offsetHeight;
  const afterLayout = box.checked;
  const matchesChecked = box.matches(':checked');
  box.setAttribute('checked', '');
  return [afterLayout, matchesChecked, box.checked, box.matches(':checked')];
})()";

// https://html.spec.whatwg.org/multipage/input.html#radio-button-group
const TWO_CHECKED_RADIOS: &str = "(() => {
  const form = document.createElement('form');
  form.innerHTML = '<input id=a type=radio name=r checked>' +
    '<input id=b type=radio name=r checked>';
  document.body.append(form);
  return [form.querySelector('#a').checked, form.querySelector('#b').checked];
})()";

// https://html.spec.whatwg.org/multipage/forms.html#concept-form-reset
// https://html.spec.whatwg.org/multipage/form-elements.html#the-output-element
const CANCELED_RESET: &str = "(() => {
  const form = document.createElement('form');
  form.innerHTML = '<output id=o>abc</output><output id=m><b>x</b></output>';
  document.body.append(form);
  const output = form.querySelector('#o');
  const markup = form.querySelector('#m');
  output.value = 'heya';
  const cancel = event => event.preventDefault();
  form.addEventListener('reset', cancel);
  form.reset();
  const canceled = [output.textContent, output.defaultValue];
  form.removeEventListener('reset', cancel);
  form.reset();
  return [
    canceled[0], canceled[1],
    output.textContent, output.defaultValue,
    markup.textContent, markup.childNodes.length, markup.firstChild.nodeType,
  ];
})()";

#[test]
fn control_state_follows_markup_insertion_and_activation() {
    let (listener, page) = server();
    TestServer::serve_on(listener, vec![("/", ok("text/html", PAGE))]);
    let (_fixture, mut client, session) =
        open_page("tinybrowser-forms", &format!("http://{page}/"));

    let values = evaluate(&mut client, &session, SANITIZE_FRAGMENT);
    assert_eq!(
        values,
        json!(["", "50", "#000000", "", "ab", "", ""]),
        "every parser path reflects the sanitized value"
    );

    let state = evaluate(&mut client, &session, SELECT_EARLIER_OPTION);
    assert_eq!(state, json!([0, true, false]));

    let state = evaluate(&mut client, &session, ACTIVATION_AND_RESET);
    assert_eq!(state, json!([false, false, true]));

    let state = evaluate(&mut client, &session, CANCELED_RESET);
    assert_eq!(
        state,
        json!(["heya", "abc", "abc", "abc", "x", 1, 3]),
        "a canceled reset keeps the output, and an uncanceled reset restores it"
    );

    let state = evaluate(&mut client, &session, CHECKED_BEFORE_LAYOUT);
    assert_eq!(state, json!([true, true, true, true]));

    let state = evaluate(&mut client, &session, TWO_CHECKED_RADIOS);
    assert_eq!(state, json!([false, true]));
}
