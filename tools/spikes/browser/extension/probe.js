const browserApi = globalThis.browser ?? globalThis.chrome;

browserApi.runtime.sendMessage({ type: "probe" }).then(
  (result) => {
    document.querySelector("#result").textContent = JSON.stringify(result);
  },
  (error) => {
    document.querySelector("#result").textContent = JSON.stringify({ error: error.message });
  },
);
