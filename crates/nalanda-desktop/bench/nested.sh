# Source this to point Wayland clients and swaymsg at the headless benchmark sway started with
#   WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WAYLAND_DISPLAY= sway -c crates/nalanda-desktop/bench/sway.conf
export WAYLAND_DISPLAY=$(ls -t "$XDG_RUNTIME_DIR" | grep -m1 '^wayland-[0-9]*$')
export SWAYSOCK=$(ls -t "$XDG_RUNTIME_DIR"/sway-ipc.*.sock | head -1)
unset DISPLAY
