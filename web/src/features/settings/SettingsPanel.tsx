import { formatAge } from "../overview/format";
import type { AreaSectionView, SettingsAreaDef } from "./areas";
import { SettingField } from "./SettingField";
import type { LastChangeDto, SettingDto, SettingsResponse } from "./types";

interface SettingsPanelProps {
  area: SettingsAreaDef;
  sections: readonly AreaSectionView[];
  data: SettingsResponse;
  drafts: Record<string, string>;
  errors: Record<string, string>;
  latestChange: LastChangeDto | null;
  onChange: (spec: SettingDto, raw: string) => void;
}

/** The right-hand panel: fields and exactly one save for this area view. */
export function SettingsPanel({
  area,
  sections,
  data,
  drafts,
  errors,
  latestChange,
  onChange,
}: SettingsPanelProps) {
  const footerNote = latestChange
    ? `最近变更 ${formatAge(latestChange.ts_ms)} · ${latestChange.actor ?? "系统"}`
    : "本区暂无变更记录";

  return (
    <section className="st-panel" aria-label={`${area.label}参数`} data-testid="settings-panel">
      {sections.length === 0 ? (
        <p className="st-help">该区域暂无可用字段。</p>
      ) : (
        sections.map((section) => (
          <div className="st-panel-section" key={section.id}>
            <h3 className="st-subhead">{section.title}</h3>
            <div className="st-fields">
              {section.specs.map((spec) => (
                <SettingField
                  key={spec.key}
                  spec={spec}
                  value={data.values[spec.key]}
                  draft={drafts[spec.key]}
                  error={errors[spec.key]}
                  onChange={(raw) => onChange(spec, raw)}
                />
              ))}
            </div>
          </div>
        ))
      )}

      <div className="st-panel-foot">
        <span className="st-saved">{footerNote}</span>
      </div>
    </section>
  );
}
