// FHIR base URL (same origin)
const FHIR_BASE = "/Patient";

// Auth header — uses default basic auth from config.yaml
const AUTH_HEADERS = {
  Authorization: "Basic " + btoa("doctor:doctor"),
};

const form = document.getElementById("patient-form");
const resultDiv = document.getElementById("result");
const tbody = document.getElementById("patient-tbody");
const noPatients = document.getElementById("no-patients");
const submitBtn = document.getElementById("submit-btn");

// Build a FHIR Patient resource from form values
function buildPatient() {
  const family = document.getElementById("family").value.trim();
  const given = document.getElementById("given").value.trim();
  const gender = document.getElementById("gender").value;
  const birthDate = document.getElementById("birthdate").value;
  const phone = document.getElementById("phone").value.trim();
  const email = document.getElementById("email").value.trim();
  const address = document.getElementById("address").value.trim();

  const patient = {
    resourceType: "Patient",
    name: [
      {
        use: "official",
        family: family,
        given: [given],
      },
    ],
    gender: gender,
  };

  if (birthDate) {
    patient.birthDate = birthDate;
  }

  const telecoms = [];
  if (phone) {
    telecoms.push({ system: "phone", value: phone, use: "home" });
  }
  if (email) {
    telecoms.push({ system: "email", value: email });
  }
  if (telecoms.length > 0) {
    patient.telecom = telecoms;
  }

  if (address) {
    patient.address = [{ use: "home", text: address }];
  }

  return patient;
}

// Show result message
function showResult(message, isError) {
  resultDiv.textContent = message;
  resultDiv.className = "result " + (isError ? "error" : "success");
  resultDiv.classList.remove("hidden");
  setTimeout(() => resultDiv.classList.add("hidden"), 5000);
}

// Register patient via FHIR API
async function registerPatient(patient) {
  const res = await fetch(FHIR_BASE, {
    method: "POST",
    headers: {
      "Content-Type": "application/fhir+json",
      ...AUTH_HEADERS,
    },
    body: JSON.stringify(patient),
  });

  if (!res.ok) {
    const body = await res.json().catch(() => null);
    const msg =
      body?.issue?.[0]?.diagnostics || body?.issue?.[0]?.details?.text || res.statusText;
    throw new Error(msg);
  }

  return res.json();
}

// Fetch and display patient list
async function loadPatients() {
  try {
    const res = await fetch(FHIR_BASE + "?_count=50&_sort=-_lastUpdated", {
      headers: AUTH_HEADERS,
    });

    if (!res.ok) return;

    const bundle = await res.json();
    const entries = bundle.entry || [];

    tbody.innerHTML = "";

    if (entries.length === 0) {
      noPatients.classList.remove("hidden");
      document.getElementById("patient-table").classList.add("hidden");
      return;
    }

    noPatients.classList.add("hidden");
    document.getElementById("patient-table").classList.remove("hidden");

    for (const entry of entries) {
      const p = entry.resource;
      const name = p.name?.[0];
      const displayName = name
        ? (name.family || "") + " " + (name.given?.join(" ") || "")
        : "-";
      const phone =
        p.telecom?.find((t) => t.system === "phone")?.value || "-";

      const tr = document.createElement("tr");
      tr.innerHTML =
        "<td>" + escapeHtml(p.id) + "</td>" +
        "<td>" + escapeHtml(displayName.trim()) + "</td>" +
        "<td>" + escapeHtml(p.gender || "-") + "</td>" +
        "<td>" + escapeHtml(p.birthDate || "-") + "</td>" +
        "<td>" + escapeHtml(phone) + "</td>";
      tbody.appendChild(tr);
    }
  } catch (e) {
    console.error("Failed to load patients:", e);
  }
}

function escapeHtml(str) {
  const div = document.createElement("div");
  div.textContent = str;
  return div.innerHTML;
}

// Form submit handler
form.addEventListener("submit", async (e) => {
  e.preventDefault();
  submitBtn.disabled = true;
  submitBtn.textContent = "Registering...";

  try {
    const patient = buildPatient();
    const created = await registerPatient(patient);
    showResult(
      "Patient registered: " + (created.id || "OK"),
      false
    );
    form.reset();
    await loadPatients();
  } catch (err) {
    showResult("Error: " + err.message, true);
  } finally {
    submitBtn.disabled = false;
    submitBtn.textContent = "Register Patient";
  }
});

// Load patients on page load
loadPatients();
