const themePreference = localStorage.getItem("rm.theme");
document.documentElement.dataset.theme = ["light", "dark"].includes(themePreference)
  ? themePreference
  : "system";
