#!/usr/bin/env ruby
# frozen_string_literal: true

require "csv"
require "pathname"

ROOT = Pathname(__dir__).parent.realpath
REFERENCE_ROOT = Pathname(ENV.fetch("PASEO_REFERENCE_ROOT", ROOT.parent.join("paseo-rewrite").to_s)).realpath
BASELINE_ROOTS = {
  "paseo" => REFERENCE_ROOT,
  "hub" => ROOT.join(".baselines/hub").realpath,
  "relay" => ROOT.join(".baselines/relay").realpath,
  "import" => ROOT.join(".baselines/import").realpath
}.freeze
REQUIRED_FIELDS = %w[capability_id lane capability baseline source_paths test_paths dependencies rust_owner evidence_method platforms known_defect status].freeze

rows = CSV.read(ROOT.join("porting/capability-matrix.csv"), headers: true)
abort "unexpected capability-matrix headers" unless rows.headers == REQUIRED_FIELDS

ids = rows.map { |row| row.fetch("capability_id") }
id_counts = ids.each_with_object(Hash.new(0)) { |id, counts| counts[id] += 1 }
duplicates = id_counts.select { |_id, count| count > 1 }.keys
abort "duplicate capability IDs: #{duplicates.join(", ")}" unless duplicates.empty?

rows.each do |row|
  id = row.fetch("capability_id")
  REQUIRED_FIELDS.each do |field|
    abort "#{id}: empty #{field}" if row.fetch(field).to_s.strip.empty?
  end

  baseline_name = row.fetch("baseline").split("@", 2).first
  baseline_root = BASELINE_ROOTS.fetch(baseline_name) { abort "#{id}: unknown baseline #{baseline_name}" }
  %w[source_paths test_paths].each do |field|
    row.fetch(field).split(";").each do |relative_path|
      path = baseline_root.join(relative_path)
      abort "#{id}: missing #{field} #{relative_path}" unless path.exist?
    end
  end

  dependencies = row.fetch("dependencies").split(";") - ["none"]
  unknown = dependencies - ids
  abort "#{id}: unknown dependencies #{unknown.join(", ")}" unless unknown.empty?
end

dependencies_by_id = rows.to_h do |row|
  [row.fetch("capability_id"), row.fetch("dependencies").split(";") - ["none"]]
end
visiting = {}
visited = {}
visit = lambda do |id, stack|
  abort "dependency cycle: #{(stack + [id]).join(" -> ")}" if visiting[id]
  return if visited[id]

  visiting[id] = true
  dependencies_by_id.fetch(id).each { |dependency| visit.call(dependency, stack + [id]) }
  visiting.delete(id)
  visited[id] = true
end
ids.each { |id| visit.call(id, []) }

detail_counts = Dir[ROOT.join("porting/details/*.txt")].sort.to_h do |path|
  [File.basename(path), File.foreach(path).count]
end
detail_total = detail_counts.values.sum
abort "detail index total #{detail_total}, expected 9248" unless detail_total == 9_248

puts "capabilities=#{rows.length} unique_ids=#{ids.uniq.length} dependency_dag=valid"
puts "source_and_test_paths=valid detail_records=#{detail_total}"
