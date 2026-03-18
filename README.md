# wl-harmonograph

Animated wallpaper for Sway/Wayland that draws
[harmonograph](https://en.wikipedia.org/wiki/Harmonograph) patterns in the
background. A harmonograph simulates the motion of a damped pendulum system -
two pendulums control the X axis and two control the Y axis, each with their
own frequency, phase, amplitude, and damping. The interference between these
pendulums traces out intricate, slowly decaying curves.

When a pattern finishes, the screen clears and a new one begins with fresh
random parameters and a different color, resulting in a unique wallpaper on
every restart.

<p align="center">
  <img src="https://github.com/user-attachments/assets/c6d704e0-d39b-4620-974b-209fdba3255a" width="30%" />
  <img src="https://github.com/user-attachments/assets/0a79ecc3-0fce-4a1c-9e63-c52ecebd2e21" width="30%" />
  <img src="https://github.com/user-attachments/assets/a0058140-5458-46e5-a5ae-8ae9ffd65737" width="30%" />
</p>

## Architecture

GPU-accelerated rendering using EGL + OpenGL ES 2.0 on top of
smithay-client-toolkit with wlr-layer-shell:

- Curve segments are rasterized on the GPU as triangle strips into an FBO
  that accumulates over time
- Each tick the CPU computes only 3 pendulum positions and submits 3
  triangle-strip draw calls
- Catmull-Rom spline interpolation for smooth anti-aliased curves
- Supports multiple monitors at native resolution
- Minimal CPU usage (~0.5% at 10fps)

## Usage

### Sway

Remove any existing `output * bg ...` or `exec swaybg` lines and add to your
Sway config (`~/.config/sway/config`):

```
exec wl-harmonograph
```

### Install

```bash
nix run github:Mic92/wl-harmonograph
```

Or:

```bash
nix profile install github:Mic92/wl-harmonograph
```

### Configuration

Colors can be customized with environment variables:

```bash
# Foreground colors (comma-separated hex, cycles through them)
export HARMONOGRAPH_FG="#fb4934,#b8bb26,#fe8019"

# Background color
export HARMONOGRAPH_BG="#1d2021"
```

Default colors are gruvbox-inspired.

## Requirements

- A Wayland compositor supporting `wlr-layer-shell-unstable-v1` (Sway, Hyprland, etc.)
- OpenGL ES 2.0 capable GPU

## License

MIT

---

This project is a rewrite of my old [wallpaper-generator](https://github.com/pinpox/wallpaper-generator).
