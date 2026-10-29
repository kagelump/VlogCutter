//! A self-contained, local-only player for short comparison previews.

pub(super) fn player_html(variants: &[(String, String)]) -> String {
    let json = serde_json::to_string(variants)
        .expect("comparison player variant data is serializable")
        .replace('&', "\\u0026")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029");

    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Local video comparison</title>
<style>
:root {{ color-scheme: light dark; font: 16px/1.45 system-ui, sans-serif; }}
body {{ max-width: 960px; margin: 2rem auto; padding: 0 1rem; }}
h1 {{ font-size: 1.4rem; }}
video {{ display: block; width: 100%; max-height: 70vh; background: #111; }}
.controls {{ display: flex; flex-wrap: wrap; gap: .65rem; margin: 1rem 0; }}
button {{ min-height: 2.75rem; padding: .55rem .85rem; font: inherit; cursor: pointer; }}
button[aria-pressed="true"] {{ outline: 3px solid #4b9; outline-offset: 2px; }}
label {{ display: inline-flex; align-items: center; gap: .5rem; min-height: 2.75rem; }}
#current {{ font-weight: 600; }}
</style>
</head>
<body>
<main>
<h1>Local video comparison</h1>
<p id="current" aria-live="polite">Choose a variant to begin. Playback is not started automatically.</p>
<video id="player" controls playsinline preload="metadata" aria-label="Selected comparison video"></video>
<section aria-label="Comparison playback controls">
<div id="variants" class="controls" aria-label="Choose a variant"></div>
<div class="controls">
<button id="sequence" type="button">Play sequence (all variants)</button>
<label><input id="repeat" type="checkbox"> Repeat sequence</label>
<button id="stop" type="button">Stop</button>
</div>
</section>
</main>
<script>
"use strict";
const variants = {json};
const player = document.getElementById("player");
const choices = document.getElementById("variants");
const current = document.getElementById("current");
const repeat = document.getElementById("repeat");
let selected = -1;
let sequencing = false;

function selectVariant(index) {{
  sequencing = false;
  player.pause();
  selected = index;
  player.src = variants[index][1];
  player.load();
  current.textContent = "Selected variant: " + variants[index][0];
  for (const button of choices.children) {{
    button.setAttribute("aria-pressed", String(Number(button.dataset.index) === index));
  }}
}}

variants.forEach((variant, index) => {{
  const button = document.createElement("button");
  button.type = "button";
  button.dataset.index = String(index);
  button.setAttribute("aria-pressed", "false");
  button.textContent = String.fromCharCode(65 + index) + ": " + variant[0];
  button.addEventListener("click", () => selectVariant(index));
  choices.appendChild(button);
}});

document.getElementById("sequence").addEventListener("click", () => {{
  if (variants.length === 0) return;
  sequencing = true;
  selectForSequence(0);
}});

function selectForSequence(index) {{
  selected = index;
  player.pause();
  player.src = variants[index][1];
  player.load();
  current.textContent = "Playing sequence: " + String.fromCharCode(65 + index) + " — " + variants[index][0];
  for (const button of choices.children) {{
    button.setAttribute("aria-pressed", String(Number(button.dataset.index) === index));
  }}
  player.play().catch(() => {{
    sequencing = false;
    current.textContent = "Playback could not start. Use the native video controls to try again.";
  }});
}}

player.addEventListener("ended", () => {{
  if (!sequencing) return;
  if (selected + 1 < variants.length) {{
    selectForSequence(selected + 1);
  }} else if (repeat.checked) {{
    selectForSequence(0);
  }} else {{
    sequencing = false;
    current.textContent = "Sequence complete.";
  }}
}});

document.getElementById("stop").addEventListener("click", () => {{
  sequencing = false;
  player.pause();
  if (player.readyState > 0) player.currentTime = 0;
  current.textContent = selected < 0 ? "Playback stopped." : "Stopped: " + variants[selected][0];
}});
</script>
</body>
</html>
"#
    )
}

#[cfg(test)]
mod tests {
    use super::player_html;

    #[test]
    fn embeds_untrusted_variant_names_and_urls_as_safe_json_data() {
        let name = "</script><script>alert('name')</script>".to_owned();
        let url = "file:///tmp/</script><script>alert('url')</script>.mp4".to_owned();
        let html = player_html(&[
            (name.clone(), url.clone()),
            ("B".into(), "file:///tmp/b.mp4".into()),
        ]);
        assert!(!html.contains("</script><script>alert('name')"));
        assert!(!html.contains("</script><script>alert('url')"));
        assert!(html.contains("button.textContent"));
        assert!(html.contains("player.src = variants[index][1]"));
        assert!(!html.contains("innerHTML"));
        assert!(!html.contains("autoplay"));
        assert!(!html.contains("muted"));

        let start = html.find("const variants = ").unwrap() + "const variants = ".len();
        let end = html[start..].find(";\n").unwrap() + start;
        let decoded: Vec<(String, String)> = serde_json::from_str(&html[start..end]).unwrap();
        assert_eq!(decoded[0], (name, url));
        assert_eq!(decoded.len(), 2);
    }
}
