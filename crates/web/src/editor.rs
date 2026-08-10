//! Reusable rich-text editor, backed by vendored Quill 2 (BSD-3).
//!
//! Quill's script/CSS are served same-origin (the app CSP blocks CDN scripts)
//! and loaded once in the app shell. [`RichEditor`] mounts a Quill instance on a
//! unique DOM id, seeds it from `data-initial` HTML, and streams the editor HTML
//! back to a `Signal<String>` on every edit via the `dioxus.send`/`recv` bridge.
//!
//! Content stays HTML; callers that persist into the block pipeline read the
//! editor back with `QUILL_READ_JS` (in `main.rs`) so exports are unchanged.

use dioxus::prelude::*;

/// Boots a Quill 2 instance on `#%ID%`, seeds it from the element's
/// `data-initial` HTML, and streams the editor HTML back to Rust on every edit.
/// Guards against double-init and waits for the vendored Quill script to load.
#[cfg(feature = "web")]
const QUILL_INIT_JS: &str = r#"
(function(){
  const el = document.getElementById('%ID%');
  if(!el){ return; }
  function boot(){
    if(!window.Quill){ return setTimeout(boot, 40); }
    if(el.__quill){ return; }
    const q = new Quill(el, { theme:'snow', placeholder:'%PROMPT%', modules:{ toolbar: %TOOLBAR% } });
    el.__quill = q;
    // Markdown shortcuts: `# `, `## `, `> `, `- `, `1. `, `**bold**`, `` `code` ``.
    if(window.QuillMarkdown){ try { new window.QuillMarkdown(q, {}); } catch(e){} }
    const initial = el.getAttribute('data-initial');
    if(initial){ q.clipboard.dangerouslyPasteHTML(initial); }
    q.on('text-change', function(){ dioxus.send(q.root.innerHTML); });
    dioxus.send(q.root.innerHTML);
  }
  boot();
})();
"#;

/// Quill toolbar config (a JS array literal) for the reusable editor variants.
#[cfg(feature = "web")]
fn quill_toolbar(variant: &str) -> &'static str {
    match variant {
        // Comments, bios, short notes: emphasis, lists and a link.
        "light" => "[['bold','italic','underline'],[{ 'list':'bullet' },{ 'list':'ordered' }],['link','clean']]",
        // Story body / briefs — full journal-style toolbar (ref design): size,
        // colour, B/I/U/S, alignment, sub/super, lists, quote, link.
        _ => "[[{ 'header': [2, 3, false] }],[{ 'color': [] }],['bold','italic','underline','strike'],[{ 'align': '' },{ 'align': 'center' },{ 'align': 'right' }],[{ 'script': 'sub' },{ 'script': 'super' }],[{ 'list': 'ordered' },{ 'list': 'bullet' }],['blockquote','code-block'],['link','image'],['clean']]",
    }
}

/// Reusable rich-text editor backed by vendored Quill 2. Renders a mount node,
/// initialises Quill once on first render, and keeps `value` (the editor HTML) in
/// sync with edits. `variant` selects the toolbar ("full" default, or "light").
/// `id` must be unique on the page (it is the read-back / init selector). `prompt`
/// is the empty-state hint text shown in the editor.
#[component]
pub fn RichEditor(
    id: String,
    initial: String,
    value: Signal<String>,
    variant: String,
    prompt: String,
) -> Element {
    #[cfg(feature = "web")]
    {
        let dom_id = id.clone();
        let var = variant.clone();
        let hint = prompt.clone();
        use_effect(move || {
            let js = QUILL_INIT_JS
                .replace("%ID%", &dom_id)
                .replace("%TOOLBAR%", quill_toolbar(&var))
                .replace("%PROMPT%", &hint.replace('\\', "\\\\").replace('\'', "\\'").replace('\n', " "));
            let mut eval = document::eval(&js);
            let mut value = value;
            spawn(async move {
                while let Ok(html) = eval.recv::<String>().await {
                    value.set(html);
                }
            });
        });
    }
    let wrap_cls = if variant == "light" { "qed qed-sm" } else { "qed" };
    rsx! {
        div { class: "{wrap_cls}",
            div { id: "{id}", class: "qed-surface", "data-initial": "{initial}" }
        }
    }
}
