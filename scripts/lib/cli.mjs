/**
 * 發佈用 CLI 的共同結尾（REU-5）。
 *
 * scripts/ 底下這幾支腳本（fetch-baseline、probe-release、compose-latest-json、
 * compose-sha256sums）的每一種失敗，都是「刻意擋下來的發佈事故」——訊息本身
 * 才是重點。直接讓例外冒出去只會在 CI log 裡留下一坨 stack trace，真正要看的
 * 那句話還得自己撈。統一收斂成 ::error:: 註記（會浮到 GitHub Actions 的 run
 * 摘要上），並以 exit 1 結束。
 *
 * 過去這段尾巴各抄一份，而且形狀不一致（同步的 try/catch vs
 * main().catch(...)）；runCli 兩種都吃。
 */

function fail(err) {
  console.error(`::error::${err instanceof Error ? err.message : String(err)}`);
  process.exit(1);
}

/**
 * @param {() => unknown | Promise<unknown>} main 同步或非同步的進入點
 */
export function runCli(main) {
  try {
    const result = main();
    // 非同步 main：rejected promise 走同一條收斂路徑
    if (result && typeof result.then === "function") {
      Promise.resolve(result).catch(fail);
    }
  } catch (err) {
    // 同步 main 直接 throw
    fail(err);
  }
}
