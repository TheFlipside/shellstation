import React from "react";

export interface HostVerifyRequest {
  sessionId: string;
  host: string;
  port: number;
  fingerprint: string;
  keyType: string;
}

interface HostVerifyDialogProps {
  request: HostVerifyRequest;
  onRespond: (sessionId: string, accept: boolean) => void;
}

export function HostVerifyDialog({ request, onRespond }: HostVerifyDialogProps): React.JSX.Element {
  return (
    <div className="dialog-overlay" role="presentation">
      <div className="dialog" role="dialog" aria-modal="true" aria-labelledby="hv-title">
        <h3 className="dialog-title" id="hv-title">
          Verify Host Key
        </h3>
        <p className="dialog-text">
          The authenticity of host{" "}
          <strong>
            {request.host}:{request.port}
          </strong>{" "}
          cannot be established.
        </p>
        <p className="dialog-text">{request.keyType} key fingerprint:</p>
        <code className="dialog-fingerprint">{request.fingerprint}</code>
        <p className="dialog-text">Are you sure you want to continue connecting?</p>
        <div className="dialog-actions">
          <button
            type="button"
            className="dialog-btn dialog-btn-cancel"
            onClick={() => {
              onRespond(request.sessionId, false);
            }}
          >
            Reject
          </button>
          <button
            type="button"
            className="dialog-btn dialog-btn-primary"
            onClick={() => {
              onRespond(request.sessionId, true);
            }}
          >
            Accept
          </button>
        </div>
      </div>
    </div>
  );
}
