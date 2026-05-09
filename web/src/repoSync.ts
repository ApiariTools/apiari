import type { Repo } from "@apiari/types";

interface RepoSyncLabelOptions {
  includeUpstream?: boolean;
}

export function repoSyncLabel(repo: Repo, options: RepoSyncLabelOptions = {}): string {
  const { includeUpstream = false } = options;
  const ahead = repo.ahead_count ?? 0;
  const behind = repo.behind_count ?? 0;
  const upstream = repo.upstream;

  if (!upstream) return "no upstream";
  if (ahead === 0 && behind === 0) return includeUpstream ? `in sync with ${upstream}` : "in sync";
  if (ahead > 0 && behind > 0) {
    return includeUpstream
      ? `${ahead} ahead · ${behind} behind ${upstream}`
      : `${ahead} ahead · ${behind} behind`;
  }
  if (ahead > 0) return includeUpstream ? `${ahead} ahead of ${upstream}` : `${ahead} ahead`;
  return includeUpstream ? `${behind} behind ${upstream}` : `${behind} behind`;
}
