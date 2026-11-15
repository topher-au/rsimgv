# LLM Chat — Python

A persistent chat in the sidebar. Every message you send goes to an
OpenAI-compatible chat model together with the picture or video frame the viewer
is showing, its filename and its metadata. The conversation is kept across
sessions in `history.json` beside the plugin's `config.json`
(`~/.config/rsimgv/plugins/au.topher.llm-chat/`). The trash button clears it.

## Requirements

- A build of rsimgv with the `python` plugin feature, and PyGObject with GTK 4.
- A vision-capable model behind an OpenAI-compatible `/chat/completions`
  endpoint. The key is read from the environment variable named by
  `api_key_env` (`OPENAI_API_KEY` by default). With the variable unset no
  `Authorization` header is sent, which suits a local server.
- For voice: a recorder and a player on `PATH` (defaults: `parecord`, `ffplay`),
  and a speech-to-text and text-to-speech backend, below.

## What is sent

- The latest message carries the viewer's screenshot (PNG), the filename, the
  folder, the displayed size, the type, the playback position for a video, and
  the file's metadata from `metadata.get`.
- Earlier messages are sent as text, each marked with the file it was about.
  Old pictures are never resent. The last `history_messages` messages are used.
- Turn off **Send the current picture or frame** or **Send the filename and
  metadata** in the plugin settings to leave either out.

## Voice

Both directions are off by default. Each can use an HTTP endpoint or a program.

**Voice input** (`asr_backend`): the microphone button records with
`record_command` until you press it again, then transcribes.

- `http` posts the WAV to `<base>/audio/transcriptions` (the API base URL unless
  `asr_url` is set) with `asr_model`.
- `command` runs `asr_command` with `{input}` replaced by the WAV's path and reads
  the text from standard output, for example
  `whisper-cli -m /path/to/ggml-base.en.bin --no-timestamps --no-prints -f {input}`.

The transcript is sent at once, or left in the message box when **Send voice
input at once** is off.

**Spoken replies** (`tts_backend`): the speaker button, or the *Speak chat
replies* action, turns on reading each reply aloud. The stop button next to it,
or the *Stop speaking* action, silences it. Recording also silences it.

- `http` posts the reply to `<base>/audio/speech` with `tts_model` and
  `tts_voice`, asking for WAV.
- `command` runs `tts_command` with the text on standard input; it must write
  audio to `{output}`, for example `piper --model voice.onnx --output_file {output}`.

`play_command` plays the result, with `{input}` replaced by the audio file.
Markdown is stripped before speaking and code blocks are skipped.

## Limits

- The speaker button only lasts until the plugin reloads; set `auto_speak` to
  keep it on.
- A reply being streamed stops when you press the stop button, but the
  connection closes only when the server next sends a line or times out.
- Settings are read when the plugin loads.
