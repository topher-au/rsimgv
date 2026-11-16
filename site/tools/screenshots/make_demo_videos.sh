#!/bin/sh
# Make two short animated clips for the video screenshots. Usage: make_demo_videos.sh HOME_DIR
set -eu
dir="$1/Videos"
mkdir -p "$dir"
n=1
for name in sunset-timelapse city-lights; do
  ffmpeg -loglevel error -y \
    -f lavfi -i "gradients=size=1280x720:rate=30:duration=20:speed=0.0$((n + 2)):c0=0x1b2a6b:c1=0xff7e5f:c2=0x2bd9c3:c3=0x7b2ff7:nb_colors=4" \
    -f lavfi -i "sine=frequency=$((220 * n)):duration=20" \
    -c:v libx264 -pix_fmt yuv420p -c:a aac -shortest "$dir/$name.mp4"
  n=$((n + 1))
done
