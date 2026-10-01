#!/usr/bin/python3
"""Capture the Linux AT-SPI tree, focus order, and keyboard activation results."""

import importlib.util
import json
import subprocess
import sys
import time
from pathlib import Path

import pyatspi

_TREE_SPEC = importlib.util.spec_from_file_location(
    "renderer_platform_linux_tree", Path(__file__).with_name("renderer-platform-linux-tree.py")
)
tree_helpers = importlib.util.module_from_spec(_TREE_SPEC)
_TREE_SPEC.loader.exec_module(tree_helpers)


OUTPUT = Path(sys.argv[1])
WINDOW_ID = sys.argv[2]
BASELINE = Path(sys.argv[3])
MAX_TABS = 40


def text_of(node):
    try:
        text = node.queryText()
        return text.getText(0, -1) or None
    except Exception:
        return None


def states(node):
    state_set = node.getState()
    names = []
    for state_name in ("FOCUSABLE", "FOCUSED", "ENABLED", "VISIBLE", "SHOWING"):
        state = getattr(pyatspi, f"STATE_{state_name}")
        if state_set.contains(state):
            names.append(state_name.lower())
    return names


def snapshot(node, depth=0):
    item = {
        "role": node.getRoleName(),
        "name": node.name or None,
        "text": text_of(node),
        "states": states(node),
        "children": [],
    }
    if depth < 12:
        for index in range(node.childCount):
            try:
                item["children"].append(snapshot(node.getChildAtIndex(index), depth + 1))
            except Exception as error:
                item["children"].append({"error": type(error).__name__})
    return item


def flatten(node, result):
    result.append(node)
    for index in range(node.childCount):
        try:
            flatten(node.getChildAtIndex(index), result)
        except Exception:
            pass


def focused_entry(application):
    nodes = []
    flatten(application, nodes)
    focused = [node for node in nodes if node.getState().contains(pyatspi.STATE_FOCUSED)]
    if not focused:
        return None
    # Deepest focused node in document order: containers keep FOCUSED while a child holds focus.
    node = focused[-1]
    return {"role": node.getRoleName(), "name": node.name or None, "text": text_of(node)}


def all_focused(application):
    nodes = []
    flatten(application, nodes)
    return [
        {"role": node.getRoleName(), "name": node.name or None, "text": text_of(node)}
        for node in nodes
        if node.getState().contains(pyatspi.STATE_FOCUSED)
    ]


def press(key):
    subprocess.run(["xdotool", "key", key], check=True)
    time.sleep(0.2)


def names(node, result):
    if node.name:
        result.append(node.name)
    for index in range(node.childCount):
        try:
            names(node.getChildAtIndex(index), result)
        except Exception:
            pass


def dialog_names(application):
    nodes = []
    flatten(application, nodes)
    result = []
    for node in nodes:
        if node.getRoleName() == "dialog":
            names(node, result)
    return result


desktop = pyatspi.Registry.getDesktop(0)
application = None
deadline = time.monotonic() + 20
while time.monotonic() < deadline and application is None:
    for index in range(desktop.childCount):
        candidate = desktop.getChildAtIndex(index)
        if "spocky" in (candidate.name or "").lower():
            application = candidate
            break
    if application is None:
        time.sleep(0.25)
if application is None:
    raise RuntimeError("Spocky application missing from AT-SPI desktop")

# No window manager runs in the container, so focus the exact window directly.
subprocess.run(["xdotool", "windowfocus", "--sync", WINDOW_ID], check=True)
tree_before = snapshot(application)

# Walk Tab until the first focused entry returns, so repeated names stay distinct.
focus_order = []
focus_trace = [{"afterKey": None, "focusedNodes": all_focused(application)}]
returned_to_first = False
for _ in range(MAX_TABS):
    press("Tab")
    focus_trace.append({"afterKey": "Tab", "focusedNodes": all_focused(application)})
    entry = focused_entry(application)
    if focus_order and entry == focus_order[0]:
        returned_to_first = True
        break
    focus_order.append(entry)

plus_focus = focused_entry(application)
press("Return")
time.sleep(0.6)
tree_after_plus = snapshot(application)
focused_after_plus = focused_entry(application)

# Move to the pinned baseline interaction control: the Add a project action.
add_project_found = False
for _ in range(MAX_TABS):
    entry = focused_entry(application)
    if entry and "Add a project" in ((entry["name"] or "") + (entry["text"] or "")):
        add_project_found = True
        break
    press("Tab")
focused_before_add = focused_entry(application)
if add_project_found:
    press("Return")
    time.sleep(0.8)
tree_after_add = snapshot(application)
dialog_after_add = dialog_names(application)

baseline = json.loads(BASELINE.read_text(encoding="utf-8"))["comparison"]
baseline_focus = baseline["accessibility"]["desktop"]["original"]["keyboardFocus"]["entries"]
baseline_names = [entry["label"] or entry["text"] or None for entry in baseline_focus]
candidate_names = [entry["name"] if entry else None for entry in focus_order]
candidate_texts = [entry["text"] if entry else None for entry in focus_order]
baseline_dialog = baseline["interaction"]["desktop"]["original"]["after"]["dialogs"]

report = {
    "application": application.name,
    "treeBefore": tree_before,
    "focusOrder": focus_order,
    "focusTrace": focus_trace,
    "focusWalk": {
        "returnedToFirst": returned_to_first,
        "baselineNames": baseline_names,
        "candidateNames": candidate_names,
        "candidateTexts": candidate_texts,
        "namesEqual": baseline_names == candidate_names,
        "baselineRoles": [entry["role"] for entry in baseline_focus],
        "candidateRoles": [entry["role"] if entry else None for entry in focus_order],
        "normalization": "none",
    },
    "plusInteraction": {
        "input": ["Tab walk", "Return"],
        "focusedBeforeActivation": plus_focus,
        "focusedAfterActivation": focused_after_plus,
        "treeChanged": tree_before != tree_after_plus,
        "dialogObserved": tree_helpers.contains_role(tree_after_plus, "dialog"),
    },
    "addProjectInteraction": {
        "input": ["Tab until Add a project", "Return"],
        "controlFound": add_project_found,
        "activated": add_project_found,
        "focusedBeforeActivation": focused_before_add,
        "treeChanged": tree_after_plus != tree_after_add,
        "dialogObserved": bool(dialog_after_add),
        "dialogNames": dialog_after_add,
        "baselineDialogText": [item["text"] for item in baseline_dialog],
        "baselineDialogControls": [
            control["text"] for item in baseline_dialog for control in item["controls"]
        ],
    },
    "treeAfterAddProject": tree_after_add,
}
OUTPUT.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
