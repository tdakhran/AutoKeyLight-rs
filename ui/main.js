const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);

let settings = null;
let status = null;

function showError(error) {
  $("error").textContent = error ? String(error) : "";
  $("error").hidden = !error;
}

async function saveSettings(changes) {
  try {
    settings = await invoke("update_settings", { settings: { ...settings, ...changes } });
    showError(null);
  } catch (error) {
    showError(error);
  }
  renderSettings();
  // Status lines depend on settings too, like a trigger being off
  if (status) renderStatus();
}

const LIGHT_STATES = {
  on: { text: "on", className: "on" },
  off: { text: "off", className: "off" },
  unreachable: { text: "unreachable", className: "" },
  unknown: { text: "checking…", className: "" },
};

function setTracked(id, tracked) {
  const untracked = settings.untrackedLights.filter((untrackedId) => untrackedId !== id);
  saveSettings({ untrackedLights: tracked ? untracked : [...untracked, id] });
}

/// Brightness the camera turns the light on at, `null` keeps the light's own
function brightnessFor(id) {
  return settings.syncBrightness ? settings.brightness : (settings.lightBrightness[id] ?? null);
}

function setBrightness(id, brightness) {
  if (settings.syncBrightness) {
    saveSettings({ brightness });
    return;
  }

  const lightBrightness = { ...settings.lightBrightness };
  if (brightness === null) delete lightBrightness[id];
  else lightBrightness[id] = brightness;
  saveSettings({ lightBrightness });
}

// Rows by light id, so status updates change them in place instead of rebuilding the list under the cursor
const lightRows = new Map();
let renderedLights = "";

function renderLights() {
  const lights = status?.lights ?? [];

  // Only rebuild when lights come or go, or are tracked or untracked
  const key = JSON.stringify(lights.map((light) => [light.id, light.name, light.address, light.tracked]));
  if (key !== renderedLights) {
    renderedLights = key;
    buildLightRows(lights);
  }

  for (const light of lights) {
    const row = lightRows.get(light.id);
    const state = LIGHT_STATES[light.state];
    row.dot.className = `dot ${state.className}`;
    row.state.textContent = light.state === "on" && light.brightness !== null ? `on ${light.brightness}%` : state.text;
    // Don't overwrite a value that's being typed
    if (document.activeElement !== row.brightness) row.brightness.value = brightnessFor(light.id) ?? "";
  }

  $("no-lights").hidden = lights.length > 0;
}

function buildLightRows(lights) {
  const list = $("lights");
  list.replaceChildren();
  lightRows.clear();

  for (const light of lights) {
    const dot = document.createElement("span");
    dot.className = "dot";

    const name = document.createElement("span");
    name.className = "name";
    name.textContent = light.name;

    const address = document.createElement("span");
    address.className = "address";
    address.textContent = light.address;

    const label = document.createElement("span");
    label.className = "label";
    label.append(name, address);

    const state = document.createElement("span");
    state.className = "state";

    const brightness = document.createElement("input");
    brightness.type = "number";
    brightness.min = 1;
    brightness.max = 100;
    brightness.placeholder = "–";
    brightness.title = "Brightness the camera turns this light on at";
    brightness.addEventListener("change", () => {
      const value = brightness.value === "" ? null : readNumber(brightness, 1, 100);
      setBrightness(light.id, value);
    });

    const percent = document.createElement("span");
    percent.className = "brightness";
    percent.append(brightness, "%");

    const track = document.createElement("button");
    track.className = light.tracked ? "small" : "small primary";
    track.textContent = light.tracked ? "Untrack" : "Track";
    track.addEventListener("click", () => setTracked(light.id, !light.tracked));

    const item = document.createElement("li");
    item.classList.toggle("disabled", !light.tracked);
    item.append(dot, label, state, percent, track);
    list.append(item);

    lightRows.set(light.id, { dot, state, brightness });
  }
}

function renderSettings() {
  $("sync-brightness").checked = settings.syncBrightness;
  $("camera-enabled").checked = settings.cameraEnabled;
  $("calendar-enabled").checked = settings.calendarEnabled;
  $("on-delay").value = settings.cameraOnDelayMs;
  $("off-delay").value = settings.cameraOffDelayMs;
  // Don't overwrite a link that's being typed
  if (document.activeElement !== $("calendar-url")) $("calendar-url").value = settings.calendarUrl;
  $("calendar-remove").hidden = !settings.calendarUrl;
  $("calendar-refresh").value = settings.calendarRefreshMinutes;
  $("flash-count").value = settings.flashCount;
  $("flash-interval").value = settings.flashIntervalMs;
}

// "Today 14:30", "Tomorrow 09:00", or the date further out
function formatMeetingTime(iso) {
  const start = new Date(iso);
  const time = start.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  const days = Math.round((new Date(start).setHours(0, 0, 0, 0) - new Date().setHours(0, 0, 0, 0)) / 86400000);
  if (days === 0) return `Today ${time}`;
  if (days === 1) return `Tomorrow ${time}`;
  return `${start.toLocaleDateString()} ${time}`;
}

function renderCalendar() {
  const calendar = status.calendar;
  let state = "";
  let text;
  let detail = "";

  if (settings && !settings.calendarEnabled) {
    text = "Meeting reminders are off";
  } else if (!settings?.calendarUrl) {
    text = "No calendar link yet";
  } else if (calendar.error) {
    state = "off";
    text = "Couldn't load the calendar";
    detail = calendar.error;
  } else if (calendar.upcoming === null || calendar.upcoming === undefined) {
    text = "Loading the calendar…";
  } else if (calendar.nextMeeting) {
    state = "on";
    text = `Next: ${calendar.nextMeeting.summary || "Meeting"}`;
    detail = formatMeetingTime(calendar.nextMeeting.start);
  } else {
    state = "on";
    text = "No meetings in the next day";
  }

  $("calendar-dot").className = `dot ${state}`;
  setStatusText($("calendar-state"), text, detail);
}

// A status line with an optional smaller second line
function setStatusText(element, text, detail) {
  const main = document.createElement("span");
  main.className = "main";
  main.textContent = text;
  main.title = text;
  element.replaceChildren(main);
  if (detail) {
    const line = document.createElement("span");
    line.className = "detail";
    line.textContent = detail;
    element.append(line);
  }
}

function renderStatus() {
  const dot = $("camera-dot");

  if (!status.cameraSupported) {
    dot.className = "dot";
    setStatusText($("camera-state"), "Camera detection isn't supported on this system");
  } else {
    dot.className = `dot ${status.cameraOn ? "on" : "off"}`;
    const text = status.cameraOn ? `Camera is on: ${status.cameras.join(", ")}` : "Camera is off";
    setStatusText($("camera-state"), text, settings && !settings.cameraEnabled ? "Lights don't follow the camera" : "");
  }

  renderLights();
  renderCalendar();
}

function readDelay(input) {
  const value = Math.max(0, Math.round(Number(input.value) || 0));
  input.value = value;
  return value;
}

$("on-delay").addEventListener("change", (event) => saveSettings({ cameraOnDelayMs: readDelay(event.target) }));
$("off-delay").addEventListener("change", (event) => saveSettings({ cameraOffDelayMs: readDelay(event.target) }));

function readNumber(input, min, max) {
  const value = Math.min(max, Math.max(min, Math.round(Number(input.value) || min)));
  input.value = value;
  return value;
}

$("sync-brightness").addEventListener("change", (event) => {
  const changes = { syncBrightness: event.target.checked };

  // Switching either way keeps what's shown, values only change when they're edited
  const lights = status?.lights ?? [];
  if (event.target.checked) {
    // The lights may differ, the first one's value becomes the shared one
    if (lights.length > 0) changes.brightness = settings.lightBrightness[lights[0].id] ?? null;
  } else {
    // Every light starts from the shared value
    const lightBrightness = { ...settings.lightBrightness };
    for (const light of lights) {
      if (settings.brightness === null) delete lightBrightness[light.id];
      else lightBrightness[light.id] = settings.brightness;
    }
    changes.lightBrightness = lightBrightness;
  }

  saveSettings(changes);
});

$("camera-enabled").addEventListener("change", (event) => saveSettings({ cameraEnabled: event.target.checked }));
$("calendar-enabled").addEventListener("change", (event) => saveSettings({ calendarEnabled: event.target.checked }));

$("calendar-form").addEventListener("submit", (event) => {
  event.preventDefault();
  saveSettings({ calendarUrl: $("calendar-url").value.trim() });
  $("calendar-url").blur();
});

$("calendar-remove").addEventListener("click", () => {
  $("calendar-url").value = "";
  saveSettings({ calendarUrl: "" });
});

$("calendar-show").addEventListener("click", () => {
  const hidden = $("calendar-url").type === "password";
  $("calendar-url").type = hidden ? "text" : "password";
  $("calendar-show").textContent = hidden ? "Hide" : "Show";
});

$("calendar-refresh").addEventListener("change", (event) =>
  saveSettings({ calendarRefreshMinutes: readNumber(event.target, 1, 120) }),
);
$("flash-count").addEventListener("change", (event) => saveSettings({ flashCount: readNumber(event.target, 1, 10) }));
$("flash-interval").addEventListener("change", (event) =>
  saveSettings({ flashIntervalMs: readNumber(event.target, 100, 5000) }),
);
$("flash-test").addEventListener("click", () => invoke("flash_lights").catch(showError));

// Tabs, the last one used is remembered
function showTab(id) {
  for (const tab of document.querySelectorAll(".tab")) tab.classList.toggle("active", tab.dataset.tab === id);
  for (const panel of document.querySelectorAll(".tab-panel")) panel.hidden = panel.id !== id;
  try {
    localStorage.setItem("tab", id);
  } catch {}
}

for (const tab of document.querySelectorAll(".tab")) tab.addEventListener("click", () => showTab(tab.dataset.tab));

let savedTab = null;
try {
  savedTab = localStorage.getItem("tab");
} catch {}
showTab(document.getElementById(savedTab) ? savedTab : "lights-tab");

$("autostart").addEventListener("change", async (event) => {
  try {
    await invoke("set_autostart", { enabled: event.target.checked });
    showError(null);
  } catch (error) {
    showError(error);
  }
  $("autostart").checked = await invoke("get_autostart");
});

listen("status", (event) => {
  status = event.payload;
  renderStatus();
});

$("repository").addEventListener("click", (event) => {
  event.preventDefault();
  invoke("open_repository").catch(showError);
});

(async () => {
  const about = await invoke("get_about");
  $("version").textContent = `v${about.version}`;
  $("repository").title = about.repository;

  settings = await invoke("get_settings");
  status = await invoke("get_status");
  $("autostart").checked = await invoke("get_autostart");
  renderSettings();
  renderStatus();
})();
