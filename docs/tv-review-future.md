# TV review: unverified acceptance and open work

Everything in [tv-review.md](tv-review.md), [recording](tv-review-recording.md), [timing](tv-review-timing.md) and [discussions](tv-review-discussions.md) is implemented and covered by synthetic, generated or fake-transport tests. What follows has **not** been established on real hardware or real footage, plus work that is not built yet. Do not claim any of it in a status report.

## Not verified (needs real devices or footage)

- **Real exported handoffs and round trips.** A real owning catalog's `edit-plan feedback record` round trip from a discussion export (needs a completed preview artifact on private media; only type compatibility is tested). HTTPS behind Caddy/Tailscale. Any YouTube or TV playback.
- **Recording.** Real browser/device capture behavior, applied audio-processing flags, sample rates, background-tab behavior, device loss and long sessions. Checklist below.
- **Transcription.** Real Whisper or OpenRouter decoding of English/Mandarin code-switching conversation; per-model WER/CER; diarization accuracy; film dialogue behind reviewers, quiet passages and real room audio; concurrent memory use beside proxy work on the server. A licensed public two-speaker code-switching fixture does not exist in the repository.
- **Live transcription.** Room-microphone quality, Mandarin accuracy, diarization and deployed-host behavior. Model latency can be measured with the ignored test:

  ```sh
  RCDVLOG_SPEECH_MODEL_DIR=/absolute/model/directory \
  RCDVLOG_LIVE_TEST_WAV=/absolute/jfk.wav \
    cargo test -p rcdvlog-server-core real_live_model_latency_and_full_pass -- --ignored --nocapture
  ```

  (16 kHz mono PCM16 public speech, e.g. whisper.cpp `samples/jfk.wav`; it measures model load, 4/8/11 s draft updates and the final pass.)
- **Timing.** The whole YouTube → TV → microphone chain: acoustic delay, microphone/clock calibration, reviewer overlap, YouTube transcode, TV speakers and room acoustics. Marker audibility (A/B), false-positive rates on real recordings, and measured alignment error. Passive soundtrack and source-audio matching need an independent playback-truth capture measuring error, recovery, false positives, unmatched intervals and ambiguity; until then source-audio alignment stays experimental and opt-in, and existing real-room recordings without truth are qualitative only.
- **Discussions.** Utterance-grouping quality on real speech; real conversations and device calibration.

Real-project validation receipts and any audio stay outside Git. Record the exact candidate commit, executable digest, revision/artifact/mapping/movie identities, commands, tool versions, measured errors, coverage and failures. Compare synchronizer anchors against independently known times, label injected/cropped-reference tests separately from room/TV/microphone tests, and report unknowns and false matches.

## Not built yet

- Server-side acoustic-marker or passive-soundtrack anchors for stored sessions (detection is CLI-only today; the synchronizer does not yet consume Play + record observations for automatic comment placement).
- Bounded raw-audio playback in the editorial workspace (the session page offers the full WAV download only).
- A desktop recorder (deliberately out of scope; Chrome on the server is the recorder).
- Automatic ASR-text-to-movie matching.

## Manual real-device recording acceptance (not run)

Run against the HTTPS deployment in desktop Chrome, with a real microphone and
a real TV:

1. Refuse the microphone permission, then allow it from site settings and retry.
2. Record 10+ minutes while the TV plays. Use the manual cue on the first frame,
   and let the tab go to the background for part of the time.
3. Unplug a USB microphone mid-recording. Expect the session to end as
   `deviceLost`, with the audio captured so far kept.
4. Disable the network for 30 s mid-recording. Expect buffered chunks to upload
   after reconnection.
5. Close the tab mid-recording, reopen it, and use **Finish upload**.
6. Fill the server disk to `RCDVLOG_MIN_FREE_BYTES`. Expect 507 retries with
   the audio kept in the browser.
7. Download the WAV and confirm it plays. Delete the session, and confirm that
   the audio directory is gone and the tombstone remains.

Record the browser version, device, sample rate the context actually used,
applied processing flags, and any gaps. None of this was exercised in the
development environment.
