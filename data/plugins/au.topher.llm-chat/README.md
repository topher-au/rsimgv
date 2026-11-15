# LLM Chat — Python

A persistent chat in the sidebar, and in a card over the picture while the viewer is open
(`Ctrl+Alt+C`, or **Show or hide viewer chat** in the main menu, shows or hides that card). Both show the same conversation. Every message you send goes to an
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

## Request parameters

Besides the model, the token limit and streaming, the plugin settings hold:

- **Temperature** and **Top p**, sent when they are not empty.
- **Extra request parameters**, a JSON object merged into every request for anything else the server takes,
  such as `{"top_k": 40, "seed": 1}`. It is merged into nested objects rather than replacing them, and it
  cannot change `messages` or `stream`.
- **Thinking.** Servers differ in how thinking is switched, so the request fields for each state are settings:
  **Parameters for thinking on** and **Parameters for thinking off**, both JSON objects, merged after the extra
  parameters. The **Think** button in the chat header switches between them and remembers the choice. The
  default for off, `{"chat_template_kwargs": {"enable_thinking": false}}`, suits Qwen3 on vLLM and llama.cpp;
  an OpenAI reasoning model takes `{"reasoning_effort": "low"}`. With both empty the button is hidden. A value
  that is not valid JSON, or a number that is not a number, is reported and left out.

The model's reasoning is still shown when it thinks; turning thinking off only asks it not to.

## The viewer card's opacity

**Viewer card opacity** sets how opaque the card's background is, from 20 to 100 percent. Only the background
fades, so the text stays clear over the picture.

## Memory

The model keeps notes that are shared by every chat, in `memory.json` beside the config. They are sent with
each request, numbered, together with an instruction to save something worth keeping by writing
`<memory>a short fact</memory>` in its reply, and to forget one with `<forget>its number</forget>`. This needs
no tool calling from the server. The tags are removed from what is shown, stored and spoken, and a small
notice says what was remembered or forgotten.

The bookmark button in the chat header lists the memories, lets you add one, and forgets any. Clearing the chat
keeps them. Turn **Memory** off in the settings to stop sending and saving them; **Memories kept** sets how many
are kept, dropping the oldest.

## Messages

Your messages are on the right and the model's on the left. A message you send has a small picture of what
was sent beside it; click it to open that file in the viewer. Each message has a delete button, which also
deletes the reply to a message of yours. **Clear chat** is in the header and the main menu.

## Voice conversation

The headset button (or `Ctrl+Alt+V` in the viewer) starts a hands-free conversation. It needs a voice input
backend, and a speech backend if the replies are to be heard.

- The plugin listens to the microphone through `vad_record_command`, which writes raw 16 kHz mono 16-bit
  audio to standard output. Sound counts as speech when it is louder than **Speech loudness threshold** and
  a few times the room's own noise level, which is tracked as it changes. When you pause for **Pause that
  ends a message**, the speech is transcribed and sent, with the picture as for any message.
- The reply is read aloud, and listening starts again when it ends. Listening is paused while the model
  replies and while it speaks, so it does not hear itself. That also means you cannot interrupt it by
  speaking; press the stop button next to the speaker instead.
- While a voice conversation is on, **Voice conversation prompt** is added to the system prompt. It tells
  the model that its reply will be spoken, so it should answer briefly and in plain text.
- A transcript that is only a marker such as `[BLANK_AUDIO]` is ignored. If background noise starts
  messages, raise the threshold.
- Every reply has a button to read it aloud again, and the last one a button to ask for it again.

## Reasoning and markdown

Replies are shown as markdown. A model's reasoning is shown while it works, taken from the
`reasoning_content` or `reasoning` field of a reply, or from `<think>` tags in it, and collapses when the
answer starts. It is kept with the conversation, never sent back to the model, and never spoken. Markdown is
left out of what is spoken, and so are code blocks and web addresses.

The chat follows new messages while it is scrolled to the bottom. Scroll up to read earlier ones and it stays
where you are.

## The viewer card

Drag the card by its top bar to move it, and its bottom right corner to resize it. Double-click the top bar to
put it back. Its place is kept in `layout.json` beside the config.

## Limits

- The speaker button only lasts until the plugin reloads; set `auto_speak` to
  keep it on.
- Voice detection is by loudness, not by recognising speech, so steady loud noise can start a message.
- A reasoning block that starts without an opening `<think>` tag is shown as part of the reply.
- A reply being streamed stops when you press the stop button, but the
  connection closes only when the server next sends a line or times out.
- Settings are read when the plugin loads.
