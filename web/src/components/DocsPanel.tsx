import { useState, useEffect, useCallback, useRef } from "react";
import { FileText, Plus, Trash2, Eye, Edit3, Save, ArrowLeft } from "lucide-react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import type { Doc } from "@apiari/types";
import * as api from "@apiari/api";
import { DocumentSurface } from "@apiari/ui";
import styles from "./DocsPanel.module.css";

interface Props {
  workspace: string;
  remote?: string;
  initialSelectedDocName?: string | null;
  onSelectedDocNameChange?: (name: string | null) => void;
  openListByDefaultOnMobile?: boolean;
}

export function DocsPanel({
  workspace,
  remote,
  initialSelectedDocName = null,
  onSelectedDocNameChange,
  openListByDefaultOnMobile = false,
}: Props) {
  const [docs, setDocs] = useState<Doc[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [content, setContent] = useState("");
  const [savedContent, setSavedContent] = useState("");
  const [preview, setPreview] = useState(false);
  const [saving, setSaving] = useState(false);
  const [isMobile, setIsMobile] = useState(window.innerWidth <= 768);
  const [showDocList, setShowDocList] = useState(window.innerWidth > 768);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const selectedRef = useRef<string | null>(null);
  const loadedDocRef = useRef<string | null>(null);

  const edited = content !== savedContent;

  const loadDocs = useCallback(() => {
    api.getDocs(workspace, remote).then(setDocs);
  }, [workspace, remote]);

  useEffect(() => {
    const handleResize = () => {
      const mobile = window.innerWidth <= 768;
      setIsMobile(mobile);
      setShowDocList((current) => (mobile ? (selected ? current : true) : true));
    };
    window.addEventListener("resize", handleResize);
    return () => window.removeEventListener("resize", handleResize);
  }, [selected]);

  useEffect(() => {
    loadDocs();
    setSelected(initialSelectedDocName);
    selectedRef.current = initialSelectedDocName;
    loadedDocRef.current = null;
    setContent("");
    setSavedContent("");
    setShowDocList(true);
  }, [workspace, initialSelectedDocName, loadDocs]);

  useEffect(() => {
    let inflight = false;
    const id = setInterval(() => {
      if (inflight) return;
      inflight = true;
      api.getDocs(workspace, remote).then(setDocs).catch(() => {}).finally(() => { inflight = false; });
    }, 10_000);
    return () => clearInterval(id);
  }, [workspace, remote]);

  // Clear debounce timer on unmount or doc switch
  useEffect(() => {
    return () => {
      if (debounceRef.current) clearTimeout(debounceRef.current);
    };
  }, [selected]);

  const selectDoc = useCallback(
    (filename: string) => {
      if (debounceRef.current) clearTimeout(debounceRef.current);
      setSelected(filename);
      selectedRef.current = filename;
      onSelectedDocNameChange?.(filename);
      setPreview(false);
      if (window.innerWidth <= 768) {
        setShowDocList(false);
      }
      api.getDoc(workspace, filename, remote).then((doc) => {
        // Guard against out-of-order responses
        if (selectedRef.current !== filename) return;
        loadedDocRef.current = filename;
        setContent(doc.content || "");
        setSavedContent(doc.content || "");
      });
    },
    [workspace, remote, onSelectedDocNameChange],
  );

  useEffect(() => {
    if (docs.length === 0) return;
    if (isMobile && openListByDefaultOnMobile && !initialSelectedDocName && !selected) {
      setShowDocList(true);
      return;
    }
    if (selected && docs.some((doc) => doc.name === selected) && loadedDocRef.current === selected) return;
    const preferred = initialSelectedDocName && docs.some((doc) => doc.name === initialSelectedDocName)
      ? initialSelectedDocName
      : selected && docs.some((doc) => doc.name === selected)
        ? selected
        : docs[0].name;
    selectDoc(preferred);
  }, [docs, initialSelectedDocName, selected, selectDoc]);

  const handleSave = useCallback(async () => {
    if (!selected) return;
    setSaving(true);
    try {
      await api.saveDoc(workspace, selected, content, remote);
      setSavedContent(content);
      loadDocs();
    } finally {
      setSaving(false);
    }
  }, [workspace, selected, content, loadDocs]);

  const handleDelete = useCallback(async () => {
    if (!selected) return;
    if (!window.confirm(`Delete ${selected}?`)) return;
    await api.deleteDoc(workspace, selected, remote);
    setSelected(null);
    selectedRef.current = null;
    loadedDocRef.current = null;
    onSelectedDocNameChange?.(null);
    setContent("");
    setSavedContent("");
    loadDocs();
  }, [workspace, selected, loadDocs, onSelectedDocNameChange, remote]);

  const handleNew = useCallback(() => {
    let name = window.prompt("New document filename:");
    if (!name) return;
    if (!name.endsWith(".md")) name += ".md";
    api.saveDoc(workspace, name, "", remote).then(() => {
      loadDocs();
      selectDoc(name);
    });
  }, [workspace, loadDocs, selectDoc]);

  const openDocList = useCallback(() => {
    setShowDocList(true);
  }, []);

  const handleContentChange = useCallback(
    (value: string) => {
      setContent(value);
      if (debounceRef.current) clearTimeout(debounceRef.current);
      const currentSelected = selected;
      debounceRef.current = setTimeout(() => {
        if (currentSelected && selectedRef.current === currentSelected) {
          api.saveDoc(workspace, currentSelected, value, remote).then(() => {
            if (selectedRef.current === currentSelected) {
              setSavedContent(value);
            }
            loadDocs();
          });
        }
      }, 2000);
    },
    [workspace, selected, loadDocs],
  );

  return (
    <DocumentSurface
      sidebar={(!isMobile || showDocList) ? (
        <>
          <div className={styles.sidebarHeader}>
            <span className={styles.sidebarTitle}>Docs</span>
            <button className={styles.newBtn} onClick={handleNew}>
              <Plus size={14} />
              New
            </button>
          </div>
          {docs.map((doc) => (
            <button
              key={doc.name}
              className={`${styles.docItem} ${selected === doc.name ? styles.docItemActive : ""}`}
              onClick={() => selectDoc(doc.name)}
            >
              <FileText size={14} className={styles.docIcon} />
              <span className={styles.docName}>{doc.title}</span>
            </button>
          ))}
        </>
      ) : null}
      editor={selected && (!isMobile || !showDocList) ? (
        <div className={styles.editor}>
          <div className={styles.toolbar}>
            {isMobile && !showDocList && (
              <button
                className={styles.toolBtn}
                onClick={openDocList}
                aria-label="Back to document list"
              >
                <ArrowLeft size={16} />
              </button>
            )}
            <span className={styles.toolbarTitle}>{selected}</span>
            {edited && <span className={styles.editedBadge}>Edited</span>}
            <button
              className={`${styles.toolBtn} ${preview ? styles.toolBtnActive : ""}`}
              onClick={() => setPreview((v) => !v)}
              aria-label={preview ? "Switch to editor" : "Switch to preview"}
              aria-pressed={preview}
            >
              {preview ? <Edit3 size={16} /> : <Eye size={16} />}
            </button>
            <button className={styles.saveBtn} onClick={handleSave} disabled={saving || !edited}>
              <Save size={14} />
              Save
            </button>
            <button
              className={styles.deleteBtn}
              onClick={handleDelete}
              aria-label={`Delete ${selected}`}
              title="Delete"
            >
              <Trash2 size={16} />
            </button>
          </div>
          {preview ? (
            <div className={styles.preview}>
              <ReactMarkdown remarkPlugins={[remarkGfm]}>{content}</ReactMarkdown>
            </div>
          ) : (
            <textarea
              className={styles.textarea}
              value={content}
              onChange={(e) => handleContentChange(e.target.value)}
              spellCheck={false}
            />
          )}
        </div>
      ) : !isMobile ? (
        <div className={styles.empty}>Select a doc or create a new one</div>
      ) : null}
    />
  );
}
