# frozen_string_literal: true

# Entry point matching the gem name so `Bundler.require` (which requires the gem
# name at boot) loads the library. The public surface is the Motion module.
require_relative "kine"
