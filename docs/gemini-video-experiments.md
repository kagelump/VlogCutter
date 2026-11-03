# Provider guidance for Gemini video experiments

For agent-run/editorial experiments, prefer the Google Gemini Developer API
directly when an authorized Gemini key is available, especially for Flash-Lite video
analysis. The 2026-10-26 test sent native video with `service_tier: flex`; all
58 requests completed with usage and no transport errors, at an estimated
$0.07703 from published token rates. This estimate is not an invoice. An earlier
five-source OpenRouter image workflow cost $0.12061 versus $0.00825 for the
direct native-video workflow, but media representation, sampling and request
count changed too, so this is not an isolated provider-price comparison. Exact
weight identity between routes was not established.

- Keep the existing product behavior explicit: this experiment does not mean
  the application has a direct-Gemini adapter. Do not silently change provider
  routing or fallback behavior as part of an analysis run.
- For a direct run, use native video input and request Flex explicitly when the
  route/model supports it. Read the key from the user's authorized secret source
  (for example `~/.gemini.key`); send it only as an authentication header. Never
  print it, put it in a URL or argv, or save it in prompts, traces, catalogs or
  project files. Strip credentials and video bytes from retained request logs.
- Omit temperature, top-p and top-k for Gemini 3.5 Flash-Lite; Google's docs say
  these sampling parameters are ignored for this model family. The test found no
  quality gain attributable to removing them.
- Ask for clock-string locators and validate parsing, source-duration bounds,
  and event meaning locally. Plausible timestamps can still point to the wrong
  scene. For suspicious or high-value moments, caption a short video window
  without revealing the claim, then compare that caption to the claim separately;
  route uncertain results to human review. For long-source timing trouble, use
  short local clips and add their offsets deterministically in code.
- Treat event discovery as retrieval, not final edit-boundary selection. The
  test found mixed results from `MINIMAL` versus `LOW` thinking and no clear gain
  from `MEDIUM` video resolution. Keep atmospheric footage eligible, describe
  visible actions neutrally, and require playback review for final selects.
- Record model/version, endpoint, service tier, token usage, usage-priced cost
  estimate, latency, failures/reserves, media/prompt identity, and manual checks.
  Keep complete-usage cost estimates distinct from provider-billed receipts.
