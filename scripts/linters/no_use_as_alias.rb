def no_use_as_alias(root_dir)
  pattern = root_dir.join('crates', '**', '*.rs').to_s
  errors = Dir.glob(pattern).sort.flat_map do |path|
    relative = Pathname.new(path).relative_path_from(root_dir).to_s
    find_use_aliases(path, relative)
  end

  return true if errors.empty?

  puts 'Found `use ... as Name` aliases.'
  puts 'Renaming imports is forbidden; only unnamed imports `as _` (e.g. `use std::io::Write as _;`) are allowed:'
  errors.each do |err|
    puts "  #{err}"
  end
  false
end

USE_ALIAS_START_RE = /\A(?:pub(?:\([^)]*\))?\s+)?use\b/

def find_use_aliases(path, relative)
  errors = []
  in_use = false
  brace_depth = 0

  File.readlines(path).each_with_index do |raw, idx|
    code = raw.split('//', 2).first || raw
    stripped = code.strip

    unless in_use
      next unless stripped =~ USE_ALIAS_START_RE

      in_use = true
      brace_depth = 0
    end

    code.scan(/\bas\s+([A-Za-z_][A-Za-z0-9_]*)/) do |m|
      name = m[0]
      next if name == '_'

      errors << "#{relative}:#{idx + 1}: `as #{name}` aliasing in `use` statement"
    end

    brace_depth += code.count('{') - code.count('}')
    if brace_depth <= 0 && code.rstrip.end_with?(';')
      in_use = false
      brace_depth = 0
    end
  end

  errors
end
