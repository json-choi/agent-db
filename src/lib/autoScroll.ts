// Whether a scrollable element is close enough to its bottom edge that a newly-appended
// chunk should still auto-follow it. Used by AgentChat's message list so a token streaming
// in doesn't yank the view back down while the user has scrolled up to reread history.
export function isNearBottom(
  el: { scrollHeight: number; scrollTop: number; clientHeight: number },
  threshold: number,
): boolean {
  return el.scrollHeight - el.scrollTop - el.clientHeight <= threshold;
}
