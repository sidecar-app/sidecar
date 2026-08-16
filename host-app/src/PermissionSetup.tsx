import { invoke } from "@tauri-apps/api/core";

interface PermissionSetupProps {
  onBack?: () => void;
}

export function PermissionSetup({ onBack }: PermissionSetupProps) {
  const requestPermission = async () => {
    const granted = await invoke<boolean>("cmd_request_screen_recording");
    if (granted) {
      await invoke("cmd_relaunch");
    }
  };

  return (
    <div className="permission-denied">
      <h2>Allow Screen Recording</h2>
      <p>Sidecar needs Screen Recording permission to share your screen.</p>
      <div className="permission-buttons">
        <button
          type="button"
          className="launch-btn primary"
          onClick={requestPermission}
        >
          Grant Permission
        </button>
        {onBack && (
          <button
            type="button"
            className="launch-btn secondary"
            onClick={onBack}
          >
            Back
          </button>
        )}
      </div>
    </div>
  );
}
