import React, { useState } from "react";

interface FolderDialogProps {
  title: string;
  initialName?: string;
  onSubmit: (name: string) => void;
  onCancel: () => void;
}

export function FolderDialog({
  title,
  initialName = "",
  onSubmit,
  onCancel,
}: FolderDialogProps): React.JSX.Element {
  const [name, setName] = useState(initialName);

  const handleSubmit = (e: React.SyntheticEvent): void => {
    e.preventDefault();
    if (!name.trim()) return;
    onSubmit(name.trim());
  };

  return (
    <div className="dialog-overlay" onClick={onCancel} role="presentation">
      <div
        className="dialog"
        onClick={(e) => {
          e.stopPropagation();
        }}
        role="dialog"
        aria-modal="true"
        aria-labelledby="fd-title"
      >
        <h3 className="dialog-title" id="fd-title">
          {title}
        </h3>
        <form onSubmit={handleSubmit}>
          <div className="dialog-field">
            <label htmlFor="fd-name">Name</label>
            <input
              id="fd-name"
              type="text"
              value={name}
              onChange={(e) => {
                setName(e.target.value);
              }}
              placeholder="Folder name"
              autoFocus
            />
          </div>
          <div className="dialog-actions">
            <button type="button" className="dialog-btn dialog-btn-cancel" onClick={onCancel}>
              Cancel
            </button>
            <button type="submit" className="dialog-btn dialog-btn-primary">
              Save
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
