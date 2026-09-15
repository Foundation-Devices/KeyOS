#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
# SPDX-License-Identifier: GPL-3.0-or-later

"""Chart the power-log.csv written by the power-log test app.

Each axis group carries its own scale, so every line fills the plot height and can still
be read off in real units. Flag columns tint the background instead of drawing a line.
Writes one SVG for the whole log plus one per boot. Clicking a legend entry toggles its
series, so open the SVG directly; an <img> embed will not run the script.

    plot-power-log.py [power-log.csv]

Needs matplotlib.
"""

import csv
import datetime as dt
import io
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

import matplotlib

matplotlib.use("svg")
import matplotlib.dates as mdates  # noqa: E402
import matplotlib.pyplot as plt  # noqa: E402
from matplotlib.patches import Patch, Rectangle  # noqa: E402
from matplotlib.transforms import Bbox  # noqa: E402

TIME_COLUMN = "timestamp"
TIMEZONE, TIMEZONE_LABEL = dt.timezone.utc, "UTC"
FIGURE_SIZE = (16.0, 7.0)
# Room for the second axis on each side, in points.
SPINE_OFFSET = 58
# A jump bigger than this means the device was off, not a slow sample.
GAP_SECONDS = 120.0
NAN = float("nan")

# Columns sharing an axis share a scale, which is the point of grouping the two
# capacities: their curves are only worth comparing against the same mAh.
AXES = [
    ("battery (%)", "left", 0, [("battery_percent", "#1f77b4")]),
    (
        "capacity (mAh)",
        "left",
        SPINE_OFFSET,
        [("remaining_capacity_mah", "#d62728"), ("full_capacity_mah", "#9467bd")],
    ),
    ("voltage (mV)", "right", 0, [("voltage_mv", "#ff7f0e")]),
    ("current (mA)", "right", SPINE_OFFSET, [("current_ma", "#2ca02c")]),
]
FLAGS = [("charging", "#4caf50"), ("lcd_on", "#ffb300")]
GAP_COLOR = "#607d8b"

# Every id needs the "pl-" prefix, or it collides with the containers matplotlib names
# itself (figure_1, legend_1). A group can own several elements, so ids are indexed and
# matched by prefix. Toggling an inline style beats the style attribute already on the
# element, which a visibility attribute would not.
TOGGLE_SCRIPT = """
function toggle(i) {
    var nodes = document.querySelectorAll('[id^="pl-plot-' + i + '-"]');
    if (!nodes.length) { return; }
    var hide = nodes[0].style.display !== 'none';
    nodes.forEach(function (node) { node.style.display = hide ? 'none' : ''; });
    ['pl-swatch-', 'pl-label-'].forEach(function (prefix) {
        var el = document.getElementById(prefix + i);
        if (el) { el.style.opacity = hide ? '0.25' : '1'; }
    });
}
"""


def stamp(seconds):
    return dt.datetime.fromtimestamp(seconds, TIMEZONE)


def read_log(path):
    """Read the CSV into a column dict, mapping blank fields to NaN."""
    with open(path, newline="") as handle:
        rows = list(csv.DictReader(handle))
    if not rows:
        sys.exit(f"{path}: no data rows")

    wanted = [TIME_COLUMN] + [name for _, _, _, series in AXES for name, _ in series]
    missing = [name for name in wanted if name not in rows[0]]
    if missing:
        sys.exit(f"{path}: missing column(s): {', '.join(missing)}")

    columns = {name: [] for name in rows[0]}
    for row in rows:
        for name, raw in row.items():
            columns[name].append(float(raw) if raw else NAN)
    return columns


def split_boots(times):
    """Split indices into runs of uptime, separated by more than GAP_SECONDS."""
    breaks = [i for i in range(1, len(times)) if times[i] - times[i - 1] > GAP_SECONDS]
    edges = [0] + breaks + [len(times)]
    return list(zip(edges, edges[1:]))


def concatenate(columns, boots, names):
    """Join boots, separated by a NaN sample so the lines break over the gaps."""
    times, data = [], {name: [] for name in names}
    for position, (lo, hi) in enumerate(boots):
        if position:
            previous_end = columns[TIME_COLUMN][boots[position - 1][1] - 1]
            times.append((previous_end + columns[TIME_COLUMN][lo]) / 2)
            for name in names:
                data[name].append(NAN)
        times.extend(columns[TIME_COLUMN][lo:hi])
        for name in names:
            data[name].extend(columns[name][lo:hi])
    return times, data


def flag_runs(times, values, step):
    """Yield (start, end) for each contiguous run where the flag is set.

    A run ends one sample after the last set sample, or a flag still set when the device
    powered off would tint the whole gap.
    """
    start = None
    for index, value in enumerate(values):
        if value == 1.0 and start is None:
            start = times[index]
        elif value != 1.0 and start is not None:
            yield start, min(times[index], times[index - 1] + step)
            start = None
    if start is not None:
        yield start, times[-1] + step


def tint(axes, spans, color, **style):
    """Shade time ranges behind the lines, returning the patches."""
    return [
        axes.axvspan(stamp(start), stamp(end), color=color, linewidth=0, zorder=0, **style)
        for start, end in spans
    ]


def build_chart(columns, boots, title):
    """Draw one figure, returning it with its number of toggle groups."""
    names = [name for _, _, _, series in AXES for name, _ in series]
    times, data = concatenate(columns, boots, names + [name for name, _ in FLAGS])
    stamps = [stamp(t) for t in times]
    steps = sorted(b - a for a, b in zip(times, times[1:]))
    step = steps[len(steps) // 2] if steps else 1.0

    figure, base = plt.subplots(figsize=FIGURE_SIZE)
    base.spines["top"].set_visible(False)
    groups = []

    for position, (label, side, offset, series) in enumerate(AXES):
        axes = base if position == 0 else base.twinx()
        if axes is not base:
            axes.patch.set_visible(False)
            for spine in ("top", "bottom", "left", "right"):
                axes.spines[spine].set_visible(spine == side)
        axes.yaxis.set_label_position(side)
        axes.yaxis.set_ticks_position(side)
        axes.spines[side].set_position(("outward", offset))

        values = [v for name, _ in series for v in data[name] if v == v]
        low, high = (min(values), max(values)) if values else (0.0, 1.0)
        pad = (high - low) * 0.05 if high > low else max(abs(high) * 0.05, 1.0)
        axes.set_ylim(low - pad, high + pad)

        # A single-series axis is colour-coded to its line; a shared one cannot be.
        tone = series[0][1] if len(series) == 1 else "#333333"
        axes.set_ylabel(label, color=tone)
        axes.tick_params(axis="y", colors=tone, labelsize=8)
        axes.spines[side].set_color(tone)

        for name, color in series:
            (line,) = axes.plot(stamps, data[name], color=color, linewidth=1.2, label=name)
            artists = [line]
            # Current crosses zero, and on its own axis that crossing is a real position
            # worth marking rather than reading off the ticks.
            if low < 0 < high:
                artists.append(axes.axhline(0, color=color, linestyle=":", linewidth=0.9, alpha=0.6))
            groups.append((line, artists))

    for name, color in FLAGS:
        runs = flag_runs(times, data[name], step)
        handle = Patch(facecolor=color, alpha=0.45, label=name)
        groups.append((handle, tint(base, runs, color, alpha=0.16)))

    gaps = [
        (columns[TIME_COLUMN][boots[i - 1][1] - 1], columns[TIME_COLUMN][lo])
        for i, (lo, _) in enumerate(boots)
        if i
    ]
    if gaps:
        label = f"off ({len(gaps)} gap{'s' if len(gaps) > 1 else ''})"
        handle = Patch(facecolor=GAP_COLOR, alpha=0.5, hatch="//", label=label)
        groups.append((handle, tint(base, gaps, GAP_COLOR, alpha=0.35, hatch="//")))

    base.xaxis.set_major_formatter(mdates.DateFormatter("%H:%M", tz=TIMEZONE))
    base.xaxis.set_major_locator(mdates.AutoDateLocator(tz=TIMEZONE))
    base.set_xlabel(f"time ({TIMEZONE_LABEL})")
    base.set_title(title)
    base.grid(True, axis="x", alpha=0.25, linewidth=0.6)
    base.margins(x=0.01)

    legend = figure.legend(
        handles=[handle for handle, _ in groups],
        loc="lower center",
        bbox_to_anchor=(0.5, -0.02),
        ncols=len(groups),
        fontsize=8,
        title="click an entry to toggle it",
        title_fontsize=8,
    )
    for index, (_, artists) in enumerate(groups):
        for position, artist in enumerate(artists):
            artist.set_gid(f"pl-plot-{index}-{position}")
    for index, artist in enumerate(legend.legend_handles):
        artist.set_gid(f"pl-swatch-{index}")
    for index, text in enumerate(legend.get_texts()):
        text.set_gid(f"pl-label-{index}")

    figure.subplots_adjust(left=0.11, right=0.89, top=0.93, bottom=0.14)
    add_legend_hitboxes(figure, legend)
    return figure, len(groups)


def add_legend_hitboxes(figure, legend):
    """Cover each legend entry with a rectangle that takes the click.

    Labels are drawn as glyph outlines, so without this only the ink itself is
    hit-testable and the entries are nearly impossible to hit.
    """
    figure.canvas.draw()
    inverse = figure.transFigure.inverted()
    for index, (handle, text) in enumerate(zip(legend.legend_handles, legend.get_texts())):
        boxes = [text.get_window_extent()]
        try:
            boxes.append(handle.get_window_extent())
        except (AttributeError, RuntimeError):
            pass
        (x0, y0), (x1, y1) = inverse.transform(Bbox.union(boxes).expanded(1.08, 1.9).get_points())
        box = Rectangle(
            (x0, y0),
            x1 - x0,
            y1 - y0,
            transform=figure.transFigure,
            facecolor="none",
            edgecolor="none",
        )
        # Above the legend, whose opaque frame would otherwise take the click.
        box.set_zorder(legend.get_zorder() + 1)
        box.set_gid(f"pl-box-{index}")
        figure.add_artist(box)


def write_svg(figure, groups, path):
    """Save the figure and graft in the click handlers that toggle each series."""
    buffer = io.StringIO()
    figure.savefig(buffer, format="svg", bbox_inches="tight")
    plt.close(figure)

    ET.register_namespace("", "http://www.w3.org/2000/svg")
    ET.register_namespace("xlink", "http://www.w3.org/1999/xlink")
    tree, ids = ET.XMLID(buffer.getvalue())
    for index in range(groups):
        for prefix in ("pl-swatch-", "pl-label-", "pl-box-"):
            element = ids.get(f"{prefix}{index}")
            if element is not None:
                element.set("cursor", "pointer")
                element.set("onclick", f"toggle({index})")
                # An unfilled box is not hit-tested by default, and catching the clicks
                # that miss the glyphs is the whole point of it.
                element.set("pointer-events", "all")

    # Spliced in as text rather than as an element, because ElementTree escapes every
    # < and > in a node's text and would corrupt the script body.
    xml = ET.tostring(tree, encoding="unicode")
    script = f'<script type="text/javascript"><![CDATA[{TOGGLE_SCRIPT}]]></script>'
    close = xml.rindex("</svg>")
    path.write_text('<?xml version="1.0" encoding="utf-8"?>\n' + xml[:close] + script + xml[close:])
    print(path)


def main():
    path = Path(sys.argv[1] if len(sys.argv) > 1 else "power-log.csv")
    columns = read_log(path)
    times = columns[TIME_COLUMN]
    boots = split_boots(times)
    plural = "s" if len(boots) > 1 else ""

    title = f"{path.name}  ({len(boots)} boot{plural}, {len(times)} samples)"
    figure, groups = build_chart(columns, boots, title)
    write_svg(figure, groups, path.with_suffix(".svg"))

    for number, boot in enumerate(boots, start=1):
        start = stamp(times[boot[0]])
        title = f"{path.name}  boot {number}/{len(boots)} from {start:%Y-%m-%d %H:%M}"
        figure, groups = build_chart(columns, [boot], title)
        write_svg(figure, groups, path.with_name(f"{path.stem}-{number}.svg"))


if __name__ == "__main__":
    main()
