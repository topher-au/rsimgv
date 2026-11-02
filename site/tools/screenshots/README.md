# Screenshots

The images in `assets/screenshots/` were captured from a real rsimgv build, on synthetic demo media.

1. Build rsimgv (`./dev.sh` in its checkout is enough; this uses `builddir/`).
2. `python3 make_demo_media.py /tmp/demo/home && ./make_demo_videos.sh /tmp/demo/home`
3. `Xvfb :99 -screen 0 1400x900x24 &` and `export DISPLAY=:99`
4. Seed a 1280×800 window, then launch in the sandbox:

   ```sh
   printf 'window:\n  width: 1280\n  height: 800\nbrowse:\n  item-size: 170\nsettings:\n  directory-previews: true\n  viewer-thumbnail-bar-pinned: true\n  viewer-status-bar: true\n' > /tmp/seed.yaml
   SEED=/tmp/seed.yaml ./launch.sh /tmp/demo/home ../../../rsimgv /tmp/state /home/demo/Pictures &
   ```
5. Drive it with `xdotool key …` and capture with
   `import -window root -crop 1280x800+0+0 +repage shot.png`, then convert to WebP:
   `magick shot.png -strip -quality 90 ../../assets/screenshots/name.webp`.

Thumbnails are made in the background and need a pointer movement (`xdotool mousemove`) to start on a display
without a window manager. Mount `/proc` in the sandbox, or thumbnailing silently fails.
