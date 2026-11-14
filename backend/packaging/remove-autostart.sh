#!/bin/sh

remove_entry() {
    home_dir="$1"
    [ -n "$home_dir" ] || return 0
    rm -f "$home_dir/.config/autostart/MeshShare.desktop"
    rm -f "$home_dir/.config/MeshShare/autostart.sh"
}

remove_entry "${HOME:-}"
remove_entry "/root"
for home_dir in /home/*; do
    [ -d "$home_dir" ] && remove_entry "$home_dir"
done
