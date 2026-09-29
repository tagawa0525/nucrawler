(() => {
  // 印（bookmark・read）を付け外しする
  const mark = (id, kind, on) => fetch(`/articles/${id}/${kind}`, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: "on=" + (on ? "1" : "0"),
    redirect: "manual",
  }).then((res) => res.ok || res.type === "opaqueredirect");
  const toast = document.createElement("div");
  toast.className = "toast";
  // 振り分けの結果をスクリーンリーダーにも伝える
  toast.setAttribute("role", "status");
  toast.hidden = true;
  document.body.append(toast);
  let timer, undoLast = null;
  const hideToast = () => { toast.hidden = true; undoLast = null; };
  const notify = (text, undo) => {
    clearTimeout(timer);
    toast.textContent = text;
    undoLast = undo || null;
    if (undo) {
      const button = document.createElement("button");
      button.textContent = "元に戻す";
      button.onclick = () => { hideToast(); undo(); };
      toast.append(button);
    }
    toast.hidden = false;
    timer = setTimeout(hideToast, 6000);
  };
  const reset = (card) => {
    card.style.transform = "";
    delete card.dataset.dir;
    delete card.dataset.busy;
  };
  // 送っている間は同じカードを振り分け直さない（行動が二重に記録される）
  const triage = async (card, kind) => {
    if (card.dataset.busy) return;
    card.dataset.busy = "1";
    const id = card.dataset.id;
    // 既に付いていた印は、取り消しで外さない（付けたのはこのスワイプではない）
    const had = kind === "bookmark" ? !!card.dataset.bookmarked : card.classList.contains("read");
    card.style.transform = `translateX(${kind === "bookmark" ? "" : "-"}110%)`;
    if (!had && !(await mark(id, kind, true).catch(() => false))) {
      reset(card);
      notify("記録できませんでした");
      return;
    }
    card.hidden = true;
    notify(kind === "bookmark" ? "🔖 ブックマークしました" : "既読にしました", async () => {
      if (had || await mark(id, kind, false).catch(() => false)) {
        reset(card);
        card.hidden = false;
      } else {
        notify("取り消せませんでした");
      }
    });
  };
  const TRIAGE_KEYS = { l: "bookmark", ArrowRight: "bookmark", h: "read", ArrowLeft: "read" };
  const MOVE_KEYS = { j: 1, ArrowDown: 1, k: -1, ArrowUp: -1 };
  document.addEventListener("keydown", (e) => {
    if (e.altKey || e.ctrlKey || e.metaKey || e.target.closest("input, textarea, select")) return;
    const cards = [...document.querySelectorAll(".card[data-id]")]
      .filter((c) => !c.hidden && !c.dataset.busy);
    const current = e.target.closest(".card[data-id]");
    const at = cards.indexOf(current);
    if (e.key in MOVE_KEYS) {
      const next = cards[at < 0 ? 0 : Math.min(Math.max(at + MOVE_KEYS[e.key], 0), cards.length - 1)];
      if (next) { e.preventDefault(); next.focus(); }
    } else if (e.key in TRIAGE_KEYS && at >= 0) {
      e.preventDefault();
      // 振り分けたカードは隠れるので、隣のカードを選んでおく
      const next = cards[at + 1] || cards[at - 1];
      triage(current, TRIAGE_KEYS[e.key]);
      if (next) next.focus();
    } else if (e.key === "Enter" && e.target === current) {
      current.querySelector("a.title").click();
    } else if (e.key === "u" && undoLast) {
      e.preventDefault();
      const undo = undoLast;
      hideToast();
      undo();
    }
  });
  const EDGE = 24, START = 10, COMMIT = 0.35;
  for (const card of document.querySelectorAll(".card[data-id]")) {
    let x0 = null, y0 = 0, dx = 0, dragging = false, moved = false;
    card.addEventListener("pointerdown", (e) => {
      if (e.button !== 0 || card.dataset.busy) return;
      if (e.clientX < EDGE || e.clientX > innerWidth - EDGE) return;
      x0 = e.clientX; y0 = e.clientY; dx = 0; dragging = false; moved = false;
    });
    card.addEventListener("pointermove", (e) => {
      if (x0 === null) return;
      dx = e.clientX - x0;
      if (!dragging) {
        if (Math.abs(e.clientY - y0) > Math.abs(dx)) { x0 = null; return; }
        if (Math.abs(dx) < START) return;
        dragging = moved = true;
        card.setPointerCapture(e.pointerId);
        card.style.transition = "none";
      }
      card.style.transform = `translateX(${dx}px)`;
      card.dataset.dir = dx > 0 ? "bookmark" : "read";
    });
    const end = () => {
      const was = dragging;
      x0 = null; dragging = false;
      if (!was) return;
      card.style.transition = "";
      if (Math.abs(dx) > card.offsetWidth * COMMIT) {
        triage(card, dx > 0 ? "bookmark" : "read");
      } else {
        reset(card);
      }
    };
    card.addEventListener("pointerup", end);
    // ドラッグ中の取り消しだけを戻す（送信中のカードの状態は消さない）
    card.addEventListener("pointercancel", () => {
      const was = dragging;
      x0 = null; dragging = false;
      if (!was) return;
      card.style.transition = "";
      reset(card);
    });
    // スワイプの指を離したときのクリックで、記事を開かない
    card.addEventListener("click", (e) => {
      if (moved) { e.preventDefault(); moved = false; }
    }, true);
  }
})();
