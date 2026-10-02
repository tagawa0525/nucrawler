(() => {
  // 書き分ける選択肢（data-closed）は、閉じた選択では短く（「絞らない」は 00・★、★1〜2 を隠すは ★3+☆）書いて
  // 何の選択か分かるようにし、開いた一覧では意味が分かるように（「-」・「★1〜2 を隠す」）書く。閉じているときは
  // 選んでいない選択肢も短く書く（選択の幅は一番長い選択肢で決まるので、上部のバーに収まるように）
  // キーボードで一覧を開くキー（Alt+↓・Alt+↑・F4・Space・Enter）と、選ばずに閉じるキー
  const opens = (e) => e.key === "F4" || e.key === " " || e.key === "Enter"
    || (e.altKey && (e.key === "ArrowDown" || e.key === "ArrowUp"));
  const cancels = (e) => e.key === "Escape" || e.key === "Tab";
  for (const select of document.querySelectorAll(".bar select")) {
    const relabeled = [...select.querySelectorAll("option[data-closed]")]
      .map((option) => ({ option, label: option.textContent }));
    if (relabeled.length === 0) continue;
    const open = () => { for (const { option, label } of relabeled) option.textContent = label; };
    const close = () => { for (const { option } of relabeled) option.textContent = option.dataset.closed; };
    // 開く直前に開いた一覧の書き方に戻す（開いている一覧はその時点の文字で出る）
    select.addEventListener("pointerdown", open);
    select.addEventListener("keydown", (e) => {
      if (opens(e)) open();
      else if (cancels(e)) close();
    });
    select.addEventListener("blur", close);
    close();
  }
})();
