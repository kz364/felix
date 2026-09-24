import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { ChevronDown, ChevronUp, X } from "lucide-react";
import { useSettings } from "../../hooks/useSettings";
import { Button } from "../ui/Button";
import { Dropdown } from "../ui/Dropdown";
import { SettingContainer } from "../ui/SettingContainer";

interface PreferredMicrophonesProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const PreferredMicrophones: React.FC<PreferredMicrophonesProps> =
  React.memo(({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const {
      getSetting,
      updateSetting,
      isUpdating,
      audioDevices,
      refreshAudioDevices,
    } = useSettings();
    const preferred = getSetting("preferred_microphones") || [];
    const updating = isUpdating("preferred_microphones");
    const [toAdd, setToAdd] = useState<string | null>(null);

    useEffect(() => {
      refreshAudioDevices();
    }, [refreshAudioDevices]);

    const connected = audioDevices
      .filter((d) => !d.is_default)
      .map((d) => d.name);
    const addable = connected.filter((name) => !preferred.includes(name));

    const save = (next: string[]) =>
      updateSetting("preferred_microphones", next);

    const move = (index: number, delta: number) => {
      const next = [...preferred];
      const [item] = next.splice(index, 1);
      next.splice(index + delta, 0, item);
      save(next);
    };

    // The first connected entry is the one that will be used.
    const activeName = preferred.find((name) => connected.includes(name));

    return (
      <>
        <SettingContainer
          title={t("settings.sound.preferredMicrophones.title")}
          description={t("settings.sound.preferredMicrophones.description")}
          descriptionMode={descriptionMode}
          grouped={grouped}
        >
          <div className="flex items-center gap-2">
            <Dropdown
              options={addable.map((name) => ({ value: name, label: name }))}
              selectedValue={toAdd}
              onSelect={setToAdd}
              placeholder={t("settings.sound.preferredMicrophones.pick")}
              disabled={updating || addable.length === 0}
              onRefresh={refreshAudioDevices}
            />
            <Button
              variant="primary"
              size="md"
              disabled={!toAdd || updating}
              onClick={() => {
                if (toAdd) save([...preferred, toAdd]);
                setToAdd(null);
              }}
            >
              {t("settings.sound.preferredMicrophones.add")}
            </Button>
          </div>
        </SettingContainer>
        {preferred.length > 0 && (
          <ol className="px-4 pb-2 space-y-1">
            {preferred.map((name, index) => {
              const isConnected = connected.includes(name);
              return (
                <li key={name} className="flex items-center gap-2 text-sm">
                  <span className="w-5 text-text/50">{index + 1}.</span>
                  <span className="flex-1 truncate" title={name}>
                    {name}
                  </span>
                  <span
                    className={`text-xs ${
                      name === activeName
                        ? "text-green-500"
                        : isConnected
                          ? "text-text/60"
                          : "text-text/40"
                    }`}
                  >
                    {name === activeName
                      ? t("settings.sound.preferredMicrophones.inUse")
                      : isConnected
                        ? t("settings.sound.preferredMicrophones.connected")
                        : t("settings.sound.preferredMicrophones.disconnected")}
                  </span>
                  <button
                    type="button"
                    className="p-1 text-text/60 hover:text-text disabled:opacity-30"
                    disabled={index === 0 || updating}
                    onClick={() => move(index, -1)}
                    aria-label={t(
                      "settings.sound.preferredMicrophones.moveUp",
                      { name },
                    )}
                  >
                    <ChevronUp className="w-4 h-4" />
                  </button>
                  <button
                    type="button"
                    className="p-1 text-text/60 hover:text-text disabled:opacity-30"
                    disabled={index === preferred.length - 1 || updating}
                    onClick={() => move(index, 1)}
                    aria-label={t(
                      "settings.sound.preferredMicrophones.moveDown",
                      { name },
                    )}
                  >
                    <ChevronDown className="w-4 h-4" />
                  </button>
                  <button
                    type="button"
                    className="p-1 text-text/60 hover:text-text"
                    disabled={updating}
                    onClick={() => save(preferred.filter((n) => n !== name))}
                    aria-label={t(
                      "settings.sound.preferredMicrophones.remove",
                      { name },
                    )}
                  >
                    <X className="w-4 h-4" />
                  </button>
                </li>
              );
            })}
          </ol>
        )}
      </>
    );
  });
