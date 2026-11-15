"""AI Upscale: run an ncnn/Vulkan upscaler on the current picture.

The plugin runs ``upscayl-bin`` (or the compatible ``realesrgan-ncnn-vulkan``)
as a separate program, so none of its code is linked into rsimgv. The result is
written beside the original as ``name_x4.png`` and the original is never touched.

The program runs on a worker thread. Its result is posted back to the GTK main
thread with ``rsimgv.ui.idle_add``.
"""

import json
import os
import shutil
import subprocess
import threading

import rsimgv


PICTURE_EXTENSIONS = {".jpg", ".jpeg", ".png", ".webp"}

DEFAULTS = {
    "binary": "upscayl-bin",
    "models_dir": "",
    "model": "realesrgan-x4plus",
    "model_scale": "4",
    "output_scale": "4",
    "format": "png",
    "tile_size": 0,
    "gpu": "auto",
    "timeout_seconds": 600,
}

NAME = "AI Upscale"


class Plugin:
    def __init__(self):
        self._active = True
        self._busy = False
        self._generation = None
        self._config = dict(DEFAULTS)

    def activate(self, host):
        self._active = True
        self._generation = rsimgv.host.generation
        self._config = self._load_config()

    def deactivate(self):
        # A running upscale cannot be cancelled safely. It is ignored when it
        # finishes because the active flag and generation no longer match.
        self._active = False

    def invoke_action(self, action, context):
        if action != "upscale":
            return
        if self._busy:
            rsimgv.notify("An upscale is already running.", seconds=3)
            return

        # Read the settings again so a change on the Plugins page applies at once.
        self._config = self._load_config()

        source = self._source(context)
        if source is None:
            self._fail("there is no picture in the viewer or selected.")
            return
        if os.path.splitext(source)[1].lower() not in PICTURE_EXTENSIONS:
            self._fail("the upscaler reads only jpg, png and webp pictures.")
            return

        program = shutil.which(self._config["binary"])
        if program is None:
            self._fail(f"{self._config['binary']} was not found. Set its path on the Plugins page.")
            return

        target = self._target(source)
        command = self._command(program, source, target)

        self._busy = True
        generation = self._generation
        rsimgv.notify(f"{NAME}: upscaling {os.path.basename(source)}...", seconds=3)
        threading.Thread(
            target=self._run,
            args=(generation, command, target),
            daemon=True,
        ).start()

    # -- paths and command -----------------------------------------------

    @staticmethod
    def _source(context):
        viewer = context.get("viewer")
        if isinstance(viewer, dict) and viewer.get("path"):
            return viewer["path"]
        folder, name = context.get("path"), context.get("selected")
        if folder and name:
            return os.path.join(folder, name)
        return None

    def _target(self, source):
        """A new file beside the source that never replaces an existing one."""
        stem = os.path.splitext(source)[0]
        base = f"{stem}_x{self._config['output_scale']}"
        extension = f".{self._config['format']}"
        target, number = base + extension, 2
        while os.path.exists(target):
            target = f"{base}_{number}{extension}"
            number += 1
        return target

    def _command(self, program, source, target):
        config = self._config
        command = [
            program,
            "-i", source,
            "-o", target,
            "-n", config["model"],
            "-z", config["model_scale"],
            "-s", config["output_scale"],
            "-f", config["format"],
            "-t", str(config["tile_size"]),
            "-g", config["gpu"],
        ]
        if config["models_dir"]:
            command += ["-m", config["models_dir"]]
        return command

    # -- worker ----------------------------------------------------------

    def _run(self, generation, command, target):
        error = None
        try:
            result = subprocess.run(
                command,
                stdin=subprocess.DEVNULL,
                capture_output=True,
                text=True,
                timeout=self._config["timeout_seconds"],
            )
            if result.returncode != 0:
                # The program reports its problems on stderr.
                detail = (result.stderr or result.stdout).strip().splitlines()
                error = detail[-1] if detail else f"exit status {result.returncode}"
            elif not os.path.exists(target):
                error = "the program finished but wrote no file"
        except subprocess.TimeoutExpired:
            error = f"timed out after {self._config['timeout_seconds']} seconds"
        except OSError as exc:
            error = str(exc)

        if error is not None:
            # Do not leave a half-written file behind.
            try:
                os.remove(target)
            except FileNotFoundError:
                pass
            except OSError as exc:
                rsimgv.host.log("warning", f"cannot remove {target}: {exc}")

        rsimgv.ui.idle_add(lambda: self._finish(generation, target, error))

    def _finish(self, generation, target, error):
        if not self._active or generation != self._generation:
            return
        self._busy = False
        if error is not None:
            self._fail(error)
            return
        rsimgv.notify(f"{NAME}: saved {os.path.basename(target)}", seconds=5)
        rsimgv.action_done({"result": target})

    @staticmethod
    def _fail(message):
        rsimgv.notify(f"{NAME}: {message}", level="error", seconds=10)
        rsimgv.action_done({"error": message})

    # -- configuration ---------------------------------------------------

    def _load_config(self):
        config = dict(DEFAULTS)
        path = os.path.join(os.path.dirname(rsimgv.host.config_path()), "config.json")
        try:
            with open(path, "r", encoding="utf-8") as file:
                loaded = json.load(file)
            if isinstance(loaded, dict):
                config.update({key: value for key, value in loaded.items() if key in DEFAULTS})
        except FileNotFoundError:
            pass
        except Exception as exc:
            rsimgv.notify(f"{NAME}: cannot read {path}: {exc}", level="warning", seconds=8)

        for key in ("binary", "models_dir", "model", "model_scale", "output_scale", "format", "gpu"):
            config[key] = str(config[key])
        if config["format"] not in ("png", "jpg", "webp"):
            config["format"] = DEFAULTS["format"]
        for key in ("model_scale", "output_scale"):
            if config[key] not in ("2", "3", "4"):
                config[key] = DEFAULTS[key]
        for key, low, high in (("tile_size", 0, 4096), ("timeout_seconds", 10, 7200)):
            try:
                config[key] = max(low, min(high, int(config[key])))
            except (TypeError, ValueError):
                config[key] = DEFAULTS[key]
        # The program accepts a tile size of 0 or at least 32.
        if 0 < config["tile_size"] < 32:
            config["tile_size"] = 32
        return config
