(() => {
  // 「絞らない」の選択肢は、閉じた選択では data-closed（00・★）と書いて何の選択か分かるようにし、
  // 開いた一覧では「-」と書いてほかの選択肢と見分ける
  for (const select of document.querySelectorAll(".bar select")) {
    const blank = select.querySelector("option[data-closed]");
    if (!blank) continue;
    const dash = blank.textContent;
    const close = () => { blank.textContent = blank.selected ? blank.dataset.closed : dash; };
    // 開く直前に「-」に戻す（開いている一覧はその時点の文字で出る）
    select.addEventListener("pointerdown", () => { blank.textContent = dash; });
    select.addEventListener("blur", close);
    close();
  }
})();
