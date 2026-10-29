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

function lightState(address) {
  const light = status?.lights.find((light) => light.address === address);
  return LIGHT_STATES[light?.state ?? "unknown"];
}

function renderLights() {
  const list = $("lights");
  list.replaceChildren();

  for (const address of settings.lights) {
    const state = lightState(address);

    const dot = document.createElement("span");
    dot.className = `dot ${state.className}`;

    const label = document.createElement("span");
    label.className = "address";
    label.textContent = address;
    label.title = address;

    const stateLabel = document.createElement("span");
    stateLabel.className = "state";
    stateLabel.textContent = state.text;

    const remove = document.createElement("button");
    remove.className = "remove";
    remove.textContent = "Remove";
    remove.addEventListener("click", () =>
      saveSettings({ lights: settings.lights.filter((light) => light !== address) }),
    );

    const item = document.createElement("li");
    item.append(dot, label, stateLabel, remove);
    list.append(item);
  }

  $("no-lights").hidden = settings.lights.length > 0;
}

function renderSettings() {
  renderLights();
  $("on-delay").value = settings.cameraOnDelayMs;
  $("off-delay").value = settings.cameraOffDelayMs;
  $("manual-override").checked = settings.allowManualOverride;
}

function renderStatus() {
  const dot = $("camera-dot");
  const text = $("camera-state");

  if (!status.cameraSupported) {
    dot.className = "dot";
    text.textContent = "Camera detection isn't supported on this system";
  } else {
    dot.className = `dot ${status.cameraOn ? "on" : "off"}`;
    text.textContent = status.cameraOn ? "Camera is on" : "Camera is off";
  }

  if (settings) renderLights();
}

function readDelay(input) {
  const value = Math.max(0, Math.round(Number(input.value) || 0));
  input.value = value;
  return value;
}

$("add-light").addEventListener("submit", (event) => {
  event.preventDefault();
  const address = $("new-light").value.trim();
  if (!address) return;

  $("new-light").value = "";
  saveSettings({ lights: [...settings.lights, address] });
});

$("on-delay").addEventListener("change", (event) => saveSettings({ cameraOnDelayMs: readDelay(event.target) }));
$("off-delay").addEventListener("change", (event) => saveSettings({ cameraOffDelayMs: readDelay(event.target) }));
$("manual-override").addEventListener("change", (event) =>
  saveSettings({ allowManualOverride: event.target.checked }),
);

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

(async () => {
  settings = await invoke("get_settings");
  status = await invoke("get_status");
  $("autostart").checked = await invoke("get_autostart");
  renderSettings();
  renderStatus();
})();
