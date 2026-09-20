(() => {
  const jobs = [
    {
      id: 1,
      name: "Northern-lights-documentary.mp4",
      type: "video",
      size: "1.8 GB",
      destination: "Videos",
      progress: 62,
      speed: "8.4 MB/s",
      eta: "1 min left",
      state: "downloading",
      detail: "4 connections · HTTPS · resume supported",
    },
    {
      id: 2,
      name: "Design-assets.zip",
      type: "file",
      size: "428 MB",
      destination: "Downloads",
      progress: 24,
      speed: "3.1 MB/s",
      eta: "2 min left",
      state: "downloading",
      detail: "2 connections · HTTPS · resume supported",
    },
    {
      id: 3,
      name: "Meeting-notes.pdf",
      type: "file",
      size: "2.4 MB",
      destination: "Downloads",
      progress: 100,
      speed: "",
      eta: "Saved today at 10:42",
      state: "completed",
      detail: "Verified · saved to Downloads",
    },
  ];
  let activeFilter = "all";
  let powerMode = false;
  const $ = (selector) => document.querySelector(selector);
  const list = $("#queue-list");
  const empty = $("#empty-state");
  const live = $("#live-status");
  const qualifies = (job) =>
    activeFilter === "all" ||
    (activeFilter === "active" &&
      ["downloading", "paused"].includes(job.state)) ||
    (activeFilter === "completed" && job.state === "completed") ||
    (activeFilter === "failed" && job.state === "failed");
  const stateText = (job) =>
    job.state === "completed"
      ? `<strong class="complete">Completed</strong><span>${job.eta}</span>`
      : job.state === "paused"
        ? `<strong>Paused</strong><span>${job.progress}% complete</span>`
        : job.state === "failed"
          ? `<strong class="failed">Needs attention</strong><span>${job.eta}</span>`
          : `<strong>${job.speed}</strong><span>${job.eta}</span>`;
  const render = () => {
    const shown = jobs.filter(qualifies);
    list.innerHTML = shown
      .map(
        (job) =>
          `<article class="job" data-id="${job.id}"><div class="file-icon ${job.type === "audio" ? "audio" : ""} ${job.state === "completed" ? "done" : ""}" aria-hidden="true">${job.state === "completed" ? "✓" : job.type === "audio" ? "♫" : job.type === "video" ? "▶" : "↓"}</div><div class="job-progress"><div class="job-name" title="${job.name}">${job.name}</div><div class="job-meta">${job.size} · ${job.destination}</div><div class="progress-line" aria-label="${job.name}: ${job.progress}% complete"><span style="width:${job.progress}%"></span></div></div><div class="job-state">${stateText(job)}</div><button class="job-action" type="button" data-action="${job.state === "downloading" ? "pause" : job.state === "paused" ? "resume" : job.state === "completed" ? "open" : "retry"}" data-id="${job.id}">${job.state === "downloading" ? "Pause" : job.state === "paused" ? "Resume" : job.state === "completed" ? "Open folder" : "Retry"}</button><div class="more" aria-hidden="true">⋯</div>${powerMode ? `<div class="advanced">${job.detail}</div>` : ""}</article>`,
      )
      .join("");
    list.hidden = shown.length === 0;
    empty.hidden = shown.length !== 0;
    const active = jobs.filter((j) =>
        ["downloading", "paused"].includes(j.state),
      ).length,
      completed = jobs.filter((j) => j.state === "completed").length,
      failed = jobs.filter((j) => j.state === "failed").length;
    $("#all-count").textContent = jobs.length;
    $("#active-count").textContent = active;
    $("#complete-count").textContent = completed;
    $("#failed-count").textContent = failed;
    $("#queue-summary").textContent =
      `${active} active · ${completed} complete${failed ? ` · ${failed} needs attention` : ""}`;
  };
  const announce = (message) => {
    live.textContent = message;
  };
  document.addEventListener("click", (event) => {
    const filter = event.target.closest(".filter");
    if (filter) {
      activeFilter = filter.dataset.filter;
      document.querySelectorAll(".filter").forEach((button) => {
        const selected = button === filter;
        button.classList.toggle("active", selected);
        button.setAttribute("aria-pressed", String(selected));
      });
      render();
      return;
    }
    const action = event.target.dataset.action;
    if (action) {
      const job = jobs.find(
        (item) => item.id === Number(event.target.dataset.id),
      );
      if (action === "pause") {
        job.state = "paused";
        job.eta = `${job.progress}% complete`;
        announce(`${job.name} paused.`);
      }
      if (action === "resume") {
        job.state = "downloading";
        job.speed = "6.8 MB/s";
        job.eta = "Resuming…";
        announce(`${job.name} resumed.`);
      }
      if (action === "open")
        announce(`Open folder is simulated for ${job.name}.`);
      if (action === "retry") {
        job.state = "downloading";
        job.eta = "Retrying…";
        announce(`Retrying ${job.name}.`);
      }
      render();
    }
  });
  const dialog = $("#add-dialog");
  const openDialog = () => {
    dialog.showModal();
    $("#link-url").focus();
  };
  $("#add-link").addEventListener("click", openDialog);
  document
    .querySelectorAll(".open-add")
    .forEach((button) => button.addEventListener("click", openDialog));
  document
    .querySelectorAll("[data-close]")
    .forEach((button) =>
      button.addEventListener("click", () => dialog.close()),
    );
  $("#power-toggle").addEventListener("click", (event) => {
    powerMode = !powerMode;
    event.currentTarget.setAttribute("aria-pressed", String(powerMode));
    event.currentTarget.textContent = powerMode
      ? "Power mode: on"
      : "Power mode";
    announce(`Power mode ${powerMode ? "enabled" : "disabled"}.`);
    render();
  });
  const quality = $("#quality");
  const qualityWrap = $("#quality-wrap");
  const setMedia = () => {
    const type = document.querySelector(
      'input[name="media-type"]:checked',
    ).value;
    qualityWrap.hidden = type === "file";
    if (type === "video") {
      quality.innerHTML =
        '<option data-extension="mp4" data-size="1.2 GB">1080p · MP4 · 1.2 GB</option><option data-extension="mp4" data-size="684 MB">720p · MP4 · 684 MB</option><option data-extension="mp4" data-size="342 MB">480p · MP4 · 342 MB</option>';
    }
    if (type === "audio") {
      quality.innerHTML =
        '<option data-extension="m4a" data-size="118 MB">Best · M4A · 118 MB</option><option data-extension="mp3" data-size="95 MB">High · MP3 · 95 MB</option><option data-extension="mp3" data-size="62 MB">Standard · MP3 · 62 MB</option>';
    }
  };
  document
    .querySelectorAll('input[name="media-type"]')
    .forEach((input) => input.addEventListener("change", setMedia));
  $("#add-form").addEventListener("submit", (event) => {
    event.preventDefault();
    const url = $("#link-url").value;
    if (!url) return;
    const type = document.querySelector(
      'input[name="media-type"]:checked',
    ).value;
    const destination = $("#save-location").value;
    const selectedQuality = quality.selectedOptions[0];
    const extension =
      type === "file" ? "zip" : selectedQuality.dataset.extension;
    const size = type === "file" ? "256 MB" : selectedQuality.dataset.size;
    const title =
      type === "file" ? "New-download-file.zip" : `Source-${type}.${extension}`;
    jobs.unshift({
      id: Date.now(),
      name: title,
      type,
      size,
      destination,
      progress: 0,
      speed: "Waiting",
      eta: "Queued",
      state: "paused",
      detail: "Simulated item · source was not inspected",
    });
    dialog.close();
    event.target.reset();
    setMedia();
    activeFilter = "all";
    document.querySelector('[data-filter="all"]').click();
    announce(`${title} was added to the queue as a simulation.`);
  });
  $("#clear-completed").addEventListener("click", () => {
    const count = jobs.filter((job) => job.state === "completed").length;
    for (let i = jobs.length - 1; i >= 0; i--)
      if (jobs[i].state === "completed") jobs.splice(i, 1);
    announce(
      `${count} completed download${count === 1 ? "" : "s"} cleared from the list.`,
    );
    render();
  });
  document.addEventListener("keydown", (event) => {
    if (event.ctrlKey && event.key.toLowerCase() === "l") {
      event.preventDefault();
      if (!dialog.open) openDialog();
    }
  });
  render();
  setMedia();
})();
