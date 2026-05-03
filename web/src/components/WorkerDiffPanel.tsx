import { useMemo, useState } from "react";
import { DiffView as GitDiffView, DiffModeEnum } from "@git-diff-view/react";
import { DiffFile, getLang } from "@git-diff-view/core";
import "@git-diff-view/react/styles/diff-view-pure.css";
import styles from "./WorkerDetail.module.css";

function splitDiffByFile(raw: string): { fileName: string; content: string }[] {
  const files: { fileName: string; content: string }[] = [];
  const parts = raw.split(/^(?=diff --git )/m);
  for (const part of parts) {
    if (!part.trim()) continue;
    const headerMatch = part.match(/^diff --git a\/.+ b\/(.+)/);
    const fileName = headerMatch?.[1] ?? "unknown";
    files.push({ fileName, content: part });
  }
  return files;
}

function FileDiffView({ fileName, content }: { fileName: string; content: string }) {
  const [collapsed, setCollapsed] = useState(false);
  const diffFile = useMemo(() => {
    const lang = getLang(fileName);
    const instance = DiffFile.createInstance({
      oldFile: { fileName, fileLang: lang },
      newFile: { fileName, fileLang: lang },
      hunks: [content],
    });
    instance.initTheme("dark");
    instance.init();
    instance.buildUnifiedDiffLines();
    return instance;
  }, [fileName, content]);

  return (
    <div className={styles.diffFileSection}>
      <button
        className={styles.diffFileName}
        onClick={() => setCollapsed((current) => !current)}
        aria-expanded={!collapsed}
      >
        <span className={`${styles.chevron} ${collapsed ? styles.chevronCollapsed : ""}`}>&#9660;</span>
        {fileName}
      </button>
      {!collapsed && (
        <div className={styles.diffContent}>
          <GitDiffView
            diffFile={diffFile}
            diffViewMode={DiffModeEnum.Unified}
            diffViewTheme="dark"
            diffViewHighlight
            diffViewFontSize={12}
          />
        </div>
      )}
    </div>
  );
}

export function WorkerDiffPanel({ diff }: { diff: string }) {
  const files = useMemo(() => splitDiffByFile(diff), [diff]);
  if (files.length === 0) return <div className={styles.empty}>Empty diff</div>;
  return (
    <div>
      {files.map((file, index) => (
        <FileDiffView
          key={`${file.fileName}-${index}`}
          fileName={file.fileName}
          content={file.content}
        />
      ))}
    </div>
  );
}
