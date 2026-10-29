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
    row.state.textContent = state.text;
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

    const track = document.createElement("button");
    track.className = light.tracked ? "small" : "small primary";
    track.textContent = light.tracked ? "Untrack" : "Track";
    track.addEventListener("click", () => setTracked(light.id, !light.tracked));

    const item = document.createElement("li");
    item.classList.toggle("disabled", !light.tracked);
    item.append(dot, label, state, track);
    list.append(item);

    lightRows.set(light.id, { dot, state });
  }
}

function renderSettings() {
  $("on-delay").value = settings.cameraOnDelayMs;
  $("off-delay").value = settings.cameraOffDelayMs;
}

function renderStatus() {
  const dot = $("camera-dot");
  const text = $("camera-state");

  if (!status.cameraSupported) {
    dot.className = "dot";
    text.textContent = "Camera detection isn't supported on this system";
  } else {
    dot.className = `dot ${status.cameraOn ? "on" : "off"}`;
    text.textContent = status.cameraOn ? `Camera is on: ${status.cameras.join(", ")}` : "Camera is off";
  }

  renderLights();
}

function readDelay(input) {
  const value = Math.max(0, Math.round(Number(input.value) || 0));
  input.value = value;
  return value;
}

$("on-delay").addEventListener("change", (event) => saveSettings({ cameraOnDelayMs: readDelay(event.target) }));
$("off-delay").addEventListener("change", (event) => saveSettings({ cameraOffDelayMs: readDelay(event.target) }));

$("autostart").addEventListener("change", async (event) => {
  try {
    await invoke("set_autostart", { enabled: event.target.checked });
    showError(null);
  } catch (error) {
    showError(error);
  }
  $("autostart").checked = await invoke("get_autostart");
});

// Size the window to its content. Checked again after every resize, until the page has exactly the content's height
const content = document.querySelector("main");

// Gives up after a few tries without the content changing, rather than resizing forever if it never settles
const MAX_FIT_ATTEMPTS = 3;
let fitAttempts = 0;

function fitWindow() {
  const difference = Math.ceil(content.getBoundingClientRect().height) - window.innerHeight;
  if (difference === 0) {
    fitAttempts = 0;
    return;
  }

  if (fitAttempts >= MAX_FIT_ATTEMPTS) return;
  fitAttempts++;
  invoke("fit_window", { difference }).catch(() => {});
}

new ResizeObserver(() => {
  fitAttempts = 0;
  fitWindow();
}).observe(content);
window.addEventListener("resize", fitWindow);

listen("status", (event) => {
  status = event.payload;
  renderStatus();
});

(async () => {
  settings = await invoke("get_settings");
  status = await invoke("get_status");
  $("autostart").checked = await invoke("get_autostart");
  renderSettings();
  renderStatus();
})();
