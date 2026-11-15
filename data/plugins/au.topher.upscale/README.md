# AI Upscale — Python

Upscales the picture in the viewer, or the selected picture when the viewer is closed, with an ncnn/Vulkan
upscaler. The result is kept in rsimgv's data folder (`~/.local/share/rsimgv/upscaled/`), named by a hash of
the original's content, so it is still found after the original is moved or renamed. The original file is
never changed, and nothing is written to its folder.

The viewer shows the upscaled copy in place of the original whenever one exists. Press `U` (the
`image.toggle-upscaled` action) to switch between the two. Upscaling a picture again replaces its copy.

## Requirements

- A build of rsimgv with the `python` plugin feature enabled.
- A Vulkan-capable GPU. The upscaler does not work well on most integrated GPUs or on a CPU.
- An upscaler program that takes the Real-ESRGAN-ncnn-vulkan options:
  [`upscayl-bin`](https://github.com/upscayl/upscayl-ncnn) (the default) or `realesrgan-ncnn-vulkan`.
- Model files (`.param` and `.bin` pairs). The Upscayl app ships some, and the program's `-m` option points
  at their folder.

rsimgv runs the program as a separate process and links none of its code. Upscayl is AGPL-3.0, and each model
has its own licence, so check it before redistributing a model.

## Use

Press `Ctrl+Alt+U` or choose **Upscale picture** from the main menu. Only one upscale runs at a time, and only
jpg, png and webp pictures are accepted. Copies are PNG files. One picture is upscaled per run; a multi-item selection is not used.

## Configure

Use **Preferences → Integration → Plugins → AI Upscale**. The values are saved in
`~/.config/rsimgv/plugins/au.topher.upscale/config.json`.

| Setting | Meaning |
| --- | --- |
| `binary` | The program, as a name on `PATH` or a full path. |
| `models_dir` | Passed as `-m`. Empty uses the program's own folder. |
| `model` | Passed as `-n`, for example `realesrgan-x4plus`. |
| `model_scale` | Passed as `-z`, the scale the model was trained for. |
| `output_scale` | Passed as `-s`. A value below the model scale resamples the result. |
| `tile_size` | Passed as `-t`. 0 is automatic; lower it if the GPU runs out of memory. |
| `gpu` | Passed as `-g`, an index or `auto`. |
| `timeout_seconds` | The program is stopped after this long. |
