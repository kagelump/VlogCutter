# Synthetic proxy fixture

`proxy.v3.mp4` is a four-second generated test pattern and sine wave, with H.264 video, AAC audio and a fast-start MP4 container. It contains no original footage. The real-server Chrome suite uses it to verify decoding, playback and seeking of an attached CLI proxy without transcoding in CI.

Generated with:

```bash
ffmpeg -f lavfi -i testsrc2=size=160x90:rate=15 -f lavfi -i sine=frequency=880:sample_rate=48000 -t 4 -c:v libx264 -preset ultrafast -crf 30 -pix_fmt yuv420p -c:a aac -movflags +faststart proxy.v3.mp4
```
