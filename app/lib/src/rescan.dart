/// 変更の検知（vault_changes の rescan:true）を呼ぶ間隔。
///
/// 5 秒を基本にし、前の走査にかかった時間の 10 倍のほうが長ければ、そちらにする（走査に使う時間を、だいたい
/// 1 割までに抑える）。いちばん長くて 10 分。例：1 万ファイルの走査（約 0.03 秒）は 5 秒ごと、9.6 万フォルダの走査
/// （約 11 秒）は 110 秒ごと。
library;

/// 基本の間隔。
const Duration baseRescanInterval = Duration(seconds: 5);

/// いちばん長い間隔。
const Duration maxRescanInterval = Duration(minutes: 10);

/// 自動の走査が続けて `failures` 回失敗したときの間隔：失敗のたびに倍にする（いちばん長くて 10 分）。
Duration backoffRescanDelay(Duration delay, int failures) {
  var result = delay;
  for (var i = 0; i < failures && result < maxRescanInterval; i++) {
    result *= 2;
  }
  return result > maxRescanInterval ? maxRescanInterval : result;
}

/// 前の走査にかかった時間から、次の走査までの間隔を決める。
Duration nextRescanDelay(Duration lastScan) {
  final scaled = lastScan * 10;
  if (scaled < baseRescanInterval) {
    return baseRescanInterval;
  }
  if (scaled > maxRescanInterval) {
    return maxRescanInterval;
  }
  return scaled;
}
