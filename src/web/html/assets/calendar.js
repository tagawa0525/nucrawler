document.querySelectorAll("input[data-for]").forEach((cal) => {
  const text = cal.form.elements[cal.dataset.for];
  // 前に選んだ日付が残らないよう、開く前に毎回欄から合わせる
  const sync = () => {
    cal.value = /^\d{4}-\d{2}-\d{2}$/.test(text.value) ? text.value : "";
  };
  cal.addEventListener("focus", sync);
  cal.addEventListener("click", () => {
    sync();
    // タップで開くブラウザもあるが、PC の Chrome などは欄を押しただけでは開かない
    if (cal.showPicker) cal.showPicker();
  });
  cal.addEventListener("change", () => {
    if (cal.value) text.value = cal.value;
  });
});
