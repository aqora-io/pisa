{{- define "pisa.fullname" -}}
{{- if contains .Chart.Name .Release.Name -}}
{{- .Release.Name | trunc 50 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name .Chart.Name | trunc 50 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}

{{- define "pisa.labels" -}}
app.kubernetes.io/name: {{ .Chart.Name }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version }}
{{- end -}}

{{- define "pisa.selector" -}}
app.kubernetes.io/name: {{ .root.Chart.Name }}
app.kubernetes.io/instance: {{ .root.Release.Name }}
app.kubernetes.io/component: {{ .component }}
{{- end -}}

{{- /*
The release's worker pools, for `range`: `worker`'s own first, as an empty
map, then each of `pools`.
*/ -}}
{{- define "pisa.poolList" -}}
{{- toYaml (prepend (.Values.pools | default list) dict) -}}
{{- end -}}

{{- /*
A pool's settings, as YAML: `worker`'s, under those the pool names (`.pool`,
empty for `worker`'s own). Besides `worker`'s keys it has `suffix`, which its
names and its component label end in, and `minReplicas` and `maxReplicas`.
One of `pools` needs its `version`, which is its image's tag unless it names
one.
*/ -}}
{{- define "pisa.pool" -}}
{{- $w := mergeOverwrite (deepCopy .root.Values.worker) (deepCopy .pool) -}}
{{- $_ := set $w "suffix" "" -}}
{{- if .pool -}}
{{- $version := required "each of pools needs its version, which its image's workers serve" .pool.version -}}
{{- $_ := set $w "suffix" (printf "-%s" (.pool.name | default ($version | replace "." "-"))) -}}
{{- $_ := set $w.image "tag" (dig "image" "tag" $version .pool) -}}
{{- else if and .root.Values.pools (not $w.version) -}}
{{- fail "worker.version is required with pools: each pool scales on its own version's jobs" -}}
{{- end -}}
{{- range $bound := list "minReplicas" "maxReplicas" -}}
{{- if not (hasKey $.pool $bound) -}}
{{- $_ := set $w $bound (get $.root.Values.keda $bound) -}}
{{- end -}}
{{- end -}}
{{- toYaml $w -}}
{{- end -}}

{{- /* The job queue that the release's servers and workers share. */ -}}
{{- define "pisa.queue" -}}
{{- .Values.queue | default (include "pisa.fullname" .) -}}
{{- end -}}
