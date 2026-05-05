import { useEffect, useRef } from 'react';

export function HtmlComment({ text }: { text: string }) {
  const ref = useRef<HTMLElement>(null);
  useEffect(() => {
    if (ref.current?.parentNode) {
      const comment = document.createComment(text);
      ref.current.parentNode.replaceChild(comment, ref.current);
    }
  }, [text]);
  return <span ref={ref} />;
}
