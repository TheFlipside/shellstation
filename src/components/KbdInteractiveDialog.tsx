import React, { useCallback, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useEscapeKey } from "../hooks/useEscapeKey";

export interface KbdInteractivePrompt {
  prompt: string;
  echo: boolean;
}

export interface KbdInteractiveRequest {
  sessionId: string;
  name: string;
  instructions: string;
  prompts: KbdInteractivePrompt[];
}

interface KbdInteractiveDialogProps {
  request: KbdInteractiveRequest;
  onRespond: (sessionId: string, responses: string[]) => void;
}

export function KbdInteractiveDialog({
  request,
  onRespond,
}: KbdInteractiveDialogProps): React.JSX.Element {
  const { t } = useTranslation();
  const [values, setValues] = useState<string[]>(() => request.prompts.map(() => ""));
  const formRef = useRef<HTMLFormElement>(null);

  const handleCancel = useCallback(() => {
    onRespond(request.sessionId, []);
  }, [onRespond, request.sessionId]);

  useEscapeKey(handleCancel);

  const handleSubmit = useCallback(
    (e: React.SyntheticEvent) => {
      e.preventDefault();
      onRespond(request.sessionId, values);
    },
    [onRespond, request.sessionId, values],
  );

  const handleChange = useCallback((index: number, value: string) => {
    setValues((prev) => {
      const next = [...prev];
      next[index] = value;
      return next;
    });
  }, []);

  return (
    <div className="dialog-overlay" role="presentation">
      <div className="dialog" role="dialog" aria-modal="true" aria-labelledby="kbd-title">
        <h3 className="dialog-title" id="kbd-title">
          {request.name || t("kbdInteractive.title")}
        </h3>
        {request.instructions && <p className="dialog-text">{request.instructions}</p>}
        <form ref={formRef} onSubmit={handleSubmit}>
          {request.prompts.map((prompt, i) => (
            <div className="dialog-field" key={i}>
              <label htmlFor={`kbd-prompt-${String(i)}`}>{prompt.prompt}</label>
              <input
                id={`kbd-prompt-${String(i)}`}
                type={prompt.echo ? "text" : "password"}
                value={values[i]}
                onChange={(e) => {
                  handleChange(i, e.target.value);
                }}
                autoFocus={i === 0}
              />
            </div>
          ))}
          <div className="dialog-actions">
            <button type="button" className="dialog-btn dialog-btn-cancel" onClick={handleCancel}>
              {t("kbdInteractive.cancel")}
            </button>
            <button type="submit" className="dialog-btn dialog-btn-primary">
              {t("kbdInteractive.submit")}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
