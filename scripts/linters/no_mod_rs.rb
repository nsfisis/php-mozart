def no_mod_rs(root_dir)
  pattern = root_dir.join('crates', '*', 'src', '**', 'mod.rs').to_s
  errors = Dir.glob(pattern).sort.map do |path|
    Pathname.new(path).relative_path_from(root_dir).to_s
  end

  return true if errors.empty?

  puts 'Found `mod.rs` file(s). Use `src/<submodule>.rs` instead of `<submodule>/mod.rs`:'
  errors.each do |path|
    puts "  #{path}"
  end
  false
end
