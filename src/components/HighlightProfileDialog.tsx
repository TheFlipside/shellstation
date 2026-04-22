import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { useEscapeKey } from "../hooks/useEscapeKey";
import type { HighlightRule } from "../stores/highlightStore";

interface HighlightProfileDialogProps {
  title: string;
  initialName: string;
  initialRules: HighlightRule[];
  onSubmit: (name: string, rules: HighlightRule[]) => void;
  onCancel: () => void;
}

const DEFAULT_RULE: HighlightRule = {
  pattern: "",
  color: "#00ff00",
  case_sensitive: true,
  bold: false,
};

export function HighlightProfileDialog({
  title,
  initialName,
  initialRules,
  onSubmit,
  onCancel,
}: HighlightProfileDialogProps): React.JSX.Element {
  const { t } = useTranslation();
  useEscapeKey(onCancel);
  const [name, setName] = useState(initialName);
  const [rules, setRules] = useState(
    initialRules.length > 0 ? initialRules : [{ ...DEFAULT_RULE }],
  );

  const updateRule = (index: number, field: keyof HighlightRule, value: string | boolean): void => {
    setRules((prev) => prev.map((r, i) => (i === index ? { ...r, [field]: value } : r)));
  };

  const removeRule = (index: number): void => {
    setRules((prev) => prev.filter((_, i) => i !== index));
  };

  const addRule = (): void => {
    setRules((prev) => [...prev, { ...DEFAULT_RULE }]);
  };

  const handleSubmit = (e: React.SyntheticEvent): void => {
    e.preventDefault();
    if (!name.trim()) return;
    const validRules = rules.filter((r) => r.pattern.trim() !== "");
    onSubmit(name.trim(), validRules);
  };

  return (
    <div className="dialog-overlay" role="presentation">
      <div className="dialog dialog-wide" role="dialog">
        <h3>{title}</h3>
        <form onSubmit={handleSubmit}>
          <div className="dialog-field">
            <label htmlFor="hp-name">{t("highlighting.profileNameLabel")}</label>
            <input
              id="hp-name"
              type="text"
              value={name}
              autoFocus
              placeholder={t("highlighting.profileNamePlaceholder")}
              onChange={(e) => {
                setName(e.target.value);
              }}
            />
          </div>

          <div className="dialog-field">
            <label>{t("highlighting.rulesLabel")}</label>
            <div className="highlight-rules-list">
              {rules.map((rule, idx) => (
                <div key={idx} className="highlight-rule-row">
                  <input
                    type="text"
                    className="highlight-rule-pattern"
                    value={rule.pattern}
                    placeholder={t("highlighting.patternLabel")}
                    onChange={(e) => {
                      updateRule(idx, "pattern", e.target.value);
                    }}
                  />
                  <input
                    type="color"
                    className="highlight-rule-color"
                    value={rule.color}
                    title={t("highlighting.colorLabel")}
                    onChange={(e) => {
                      updateRule(idx, "color", e.target.value);
                    }}
                  />
                  <label
                    className="highlight-rule-checkbox"
                    title={t("highlighting.caseSensitiveLabel")}
                  >
                    <input
                      type="checkbox"
                      checked={rule.case_sensitive}
                      onChange={(e) => {
                        updateRule(idx, "case_sensitive", e.target.checked);
                      }}
                    />
                    Aa
                  </label>
                  <label className="highlight-rule-checkbox" title={t("highlighting.boldLabel")}>
                    <input
                      type="checkbox"
                      checked={rule.bold}
                      onChange={(e) => {
                        updateRule(idx, "bold", e.target.checked);
                      }}
                    />
                    <strong>B</strong>
                  </label>
                  <button
                    type="button"
                    className="highlight-rule-remove"
                    title={t("common.delete")}
                    onClick={() => {
                      removeRule(idx);
                    }}
                  >
                    &times;
                  </button>
                </div>
              ))}
            </div>
            <button type="button" className="dialog-btn dialog-btn-primary" onClick={addRule}>
              {t("highlighting.addRule")}
            </button>
          </div>

          <div className="dialog-actions">
            <button type="button" className="dialog-btn dialog-btn-cancel" onClick={onCancel}>
              {t("common.cancel")}
            </button>
            <button type="submit" className="dialog-btn dialog-btn-primary" disabled={!name.trim()}>
              {t("common.save")}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
