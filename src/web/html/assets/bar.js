(() => {
  // 「絞らない」の選択肢は、閉じた選択では data-closed（00・★）と書いて何の選択か分かるようにし、
  // 開いた一覧では「-」と書いてほかの選択肢と見分ける
  // キーボードで一覧を開くキー（Alt+↓・Alt+↑・F4・Space・Enter）と、選ばずに閉じるキー
  const opens = (e) => e.key === "F4" || e.key === " " || e.key === "Enter"
    || (e.altKey && (e.key === "ArrowDown" || e.key === "ArrowUp"));
  const cancels = (e) => e.key === "Escape" || e.key === "Tab";
  for (const select of document.querySelectorAll(".bar select")) {
    const blank = select.querySelector("option[data-closed]");
    if (!blank) continue;
    const dash = blank.textContent;
    const open = () => { blank.textContent = dash; };
    const close = () => { blank.textContent = blank.selected ? blank.dataset.closed : dash; };
    // 開く直前に「-」に戻す（開いている一覧はその時点の文字で出る）
    select.addEventListener("pointerdown", open);
    select.addEventListener("keydown", (e) => {
      if (opens(e)) open();
      else if (cancels(e)) close();
    });
    select.addEventListener("blur", close);
    close();
  }
})();
