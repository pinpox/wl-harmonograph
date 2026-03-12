#!/usr/bin/env python3
"""Animated harmonograph wallpaper for Sway/Wayland using gtk-layer-shell + Cairo."""

import math
import os
import random

import gi
gi.require_version("Gtk", "3.0")
gi.require_version("GtkLayerShell", "0.1")
from gi.repository import Gtk, Gdk, GLib, GtkLayerShell
import cairo


class Harmonograph:
    """A harmonograph defined by pendulum parameters."""

    def __init__(self):
        self.params = self._random_params()
        self._compile_params()
        self.t = 0.0
        self.max_t = 400.0
        self.step = 0.01
        self.recent_points = []

    def _random_params(self):
        """Generate random pendulum parameters for pleasing harmonograph curves."""
        base_freq = random.uniform(1.0, 2.0)
        ratios = [1, 2, 3, 4]
        # weights = [2, 2, 2, 1]  # Favor simpler ratios

        def pick_freq():
            ratio = random.choices(ratios)[0]
            detune = random.uniform(-0.03, 0.03)
            return base_freq * ratio + detune

        def pendulum(freq, primary=True):
            return {
                "amplitude": random.uniform(0.6, 1.0) if primary else random.uniform(0.1, 0.35),
                "frequency": freq,
                "phase": random.uniform(0, 2 * math.pi),
                "damping": random.uniform(0.002, 0.006),
            }

        f_x = pick_freq()
        f_y = pick_freq()
        f_x2 = pick_freq()
        f_y2 = pick_freq()

        return {
            "x1": pendulum(f_x, primary=True),
            "x2": pendulum(f_x2, primary=False),
            "y1": pendulum(f_y, primary=True),
            "y2": pendulum(f_y2, primary=False),
        }

    def _compile_params(self):
        """Extract params into flat tuples for fast evaluation."""
        p = self.params
        self._x1 = (p["x1"]["amplitude"], p["x1"]["frequency"], p["x1"]["phase"], p["x1"]["damping"])
        self._x2 = (p["x2"]["amplitude"], p["x2"]["frequency"], p["x2"]["phase"], p["x2"]["damping"])
        self._y1 = (p["y1"]["amplitude"], p["y1"]["frequency"], p["y1"]["phase"], p["y1"]["damping"])
        self._y2 = (p["y2"]["amplitude"], p["y2"]["frequency"], p["y2"]["phase"], p["y2"]["damping"])

    def _eval(self, t):
        """Evaluate harmonograph position at time t."""
        sin = math.sin
        exp = math.exp
        a, f, ph, d = self._x1
        x = a * sin(t * f + ph) * exp(-d * t)
        a, f, ph, d = self._x2
        x += a * sin(t * f + ph) * exp(-d * t)
        a, f, ph, d = self._y1
        y = a * sin(t * f + ph) * exp(-d * t)
        a, f, ph, d = self._y2
        y += a * sin(t * f + ph) * exp(-d * t)
        return x, y

    def advance(self):
        """Advance by one step. Returns the new point or None if done."""
        if self.t > self.max_t:
            return None
        x, y = self._eval(self.t)
        self.recent_points.append((x, y))
        if len(self.recent_points) > 4:
            self.recent_points.pop(0)
        self.t += self.step
        return (x, y)

    def reset(self):
        """Reset with new random parameters."""
        self.params = self._random_params()
        self._compile_params()
        self.recent_points = []
        self.t = 0.0


def parse_hex_color(hex_str):
    """Parse a hex color string (e.g. 'ff00aa') to (r, g, b) floats."""
    hex_str = hex_str.strip().lstrip("#")
    if len(hex_str) != 6:
        raise ValueError(f"Invalid hex color: {hex_str}")
    r = int(hex_str[0:2], 16) / 255.0
    g = int(hex_str[2:4], 16) / 255.0
    b = int(hex_str[4:6], 16) / 255.0
    return (r, g, b)


def colors_from_env():
    """Read HARMONOGRAPH_FG and HARMONOGRAPH_BG from environment."""
    default_fg = [
        (0.984, 0.286, 0.204),  # red
        (0.596, 0.592, 0.102),  # green/yellow
        (0.988, 0.694, 0.349),  # orange
        (0.514, 0.647, 0.596),  # aqua
        (0.827, 0.525, 0.608),  # purple
        (0.557, 0.753, 0.486),  # green
        (0.894, 0.827, 0.529),  # yellow
    ]
    default_bg = (0.114, 0.122, 0.137)

    fg_env = os.environ.get("HARMONOGRAPH_FG")
    if fg_env:
        try:
            fg = [parse_hex_color(c) for c in fg_env.split(",")]
        except ValueError as e:
            print(f"Warning: invalid HARMONOGRAPH_FG: {e}, using defaults")
            fg = default_fg
    else:
        fg = default_fg

    bg_env = os.environ.get("HARMONOGRAPH_BG")
    if bg_env:
        try:
            bg = parse_hex_color(bg_env)
        except ValueError as e:
            print(f"Warning: invalid HARMONOGRAPH_BG: {e}, using default")
            bg = default_bg
    else:
        bg = default_bg

    return fg, bg


class MonitorWindow:
    """A single background window for one monitor."""

    def __init__(self, monitor, app):
        self.app = app
        self.window = Gtk.Window()
        geom = monitor.get_geometry()
        self.mon_w = geom.width
        self.mon_h = geom.height

        GtkLayerShell.init_for_window(self.window)
        GtkLayerShell.set_layer(self.window, GtkLayerShell.Layer.BACKGROUND)
        GtkLayerShell.set_monitor(self.window, monitor)
        GtkLayerShell.set_anchor(self.window, GtkLayerShell.Edge.TOP, True)
        GtkLayerShell.set_anchor(self.window, GtkLayerShell.Edge.BOTTOM, True)
        GtkLayerShell.set_anchor(self.window, GtkLayerShell.Edge.LEFT, True)
        GtkLayerShell.set_anchor(self.window, GtkLayerShell.Edge.RIGHT, True)
        GtkLayerShell.set_exclusive_zone(self.window, -1)

        self.area = Gtk.DrawingArea()
        self.area.connect("draw", self.on_draw)
        self.window.add(self.area)
        self.window.connect("destroy", Gtk.main_quit)
        self.window.show_all()

    def on_draw(self, widget, cr):
        """Blit the shared surface, scaled to fit this monitor."""
        surface = self.app.surface
        if not surface:
            return True

        src_w = surface.get_width()
        src_h = surface.get_height()

        if src_w == self.mon_w and src_h == self.mon_h:
            # Same resolution, no scaling needed
            cr.set_source_surface(surface, 0, 0)
        else:
            # Scale to fit
            sx = self.mon_w / src_w
            sy = self.mon_h / src_h
            cr.scale(sx, sy)
            cr.set_source_surface(surface, 0, 0)

        cr.paint()
        return True


class WallpaperApp:
    """Manages harmonograph wallpaper across all monitors."""

    def __init__(self):
        self.COLORS, self.bg_color = colors_from_env()
        self.harmonograph = Harmonograph()
        self.color = random.choice(self.COLORS)
        self.windows = []
        self.surface = None
        self.surface_cr = None

        # Find the largest monitor resolution
        display = Gdk.Display.get_default()
        n_monitors = display.get_n_monitors()
        max_w, max_h = 0, 0
        monitors = []
        for i in range(n_monitors):
            monitor = display.get_monitor(i)
            monitors.append(monitor)
            geom = monitor.get_geometry()
            max_w = max(max_w, geom.width)
            max_h = max(max_h, geom.height)

        # Single surface at the largest resolution
        self.surface = cairo.ImageSurface(cairo.FORMAT_ARGB32, max_w, max_h)
        self.surface_cr = cairo.Context(self.surface)
        self.surface_cr.set_line_width(1.2)
        self.surface_cr.set_line_cap(cairo.LINE_CAP_ROUND)
        self.surface_cr.set_line_join(cairo.LINE_JOIN_ROUND)
        self.surf_w = max_w
        self.surf_h = max_h
        self.scale = min(max_w, max_h) * 0.4
        self.cx = max_w / 2
        self.cy = max_h / 2
        self._clear_surface()

        for monitor in monitors:
            self.windows.append(MonitorWindow(monitor, self))

        GLib.timeout_add(33, self.on_tick)  # ~30fps

    def _clear_surface(self):
        """Clear the surface to background color."""
        self.surface_cr.set_source_rgb(*self.bg_color)
        self.surface_cr.paint()

    def _pick_new_color(self):
        """Pick a color different from the current one."""
        available = [c for c in self.COLORS if c != self.color]
        return random.choice(available)

    def _restart(self):
        """Clear screen and start a new harmonograph."""
        self._clear_surface()
        self.harmonograph.reset()
        self.color = self._pick_new_color()

    def on_tick(self):
        """Advance simulation and draw onto the single shared surface."""
        pt = self.harmonograph.advance()
        if pt is None:
            self._restart()
            for win in self.windows:
                win.area.queue_draw()
            return True

        points = self.harmonograph.recent_points
        if len(points) < 4:
            return True

        s = self.scale
        cx = self.cx
        cy = self.cy
        cr = self.surface_cr

        p0x, p0y = points[-4][0] * s + cx, points[-4][1] * s + cy
        p1x, p1y = points[-3][0] * s + cx, points[-3][1] * s + cy
        p2x, p2y = points[-2][0] * s + cx, points[-2][1] * s + cy
        p3x, p3y = points[-1][0] * s + cx, points[-1][1] * s + cy

        cr.set_source_rgba(*self.color, 0.85)
        cr.move_to(p1x, p1y)
        cr.curve_to(
            p1x + (p2x - p0x) / 6.0, p1y + (p2y - p0y) / 6.0,
            p2x - (p3x - p1x) / 6.0, p2y - (p3y - p1y) / 6.0,
            p2x, p2y,
        )
        cr.stroke()

        for win in self.windows:
            win.area.queue_draw()
        return True


def main():
    app = WallpaperApp()
    Gtk.main()


if __name__ == "__main__":
    main()
