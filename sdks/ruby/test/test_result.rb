require_relative 'test_helper'

class TestResult < Minitest::Test
  def make_result(ok: true, data: nil, age_ms: 0, stale: false, error: nil)
    Beachcomber::Result.new(ok: ok, data: data, age_ms: age_ms, stale: stale, error: error)
  end

  # --- ok? / hit? / miss? ---

  def test_hit_when_data_present
    r = make_result(data: 'main')
    assert r.ok?
    assert r.hit?
    refute r.miss?
  end

  def test_miss_when_no_data
    r = make_result
    assert r.ok?
    refute r.hit?
    assert r.miss?
  end

  def test_error_result
    r = make_result(ok: false, error: 'unknown provider')
    refute r.ok?
    refute r.hit?
    refute r.miss?
  end

  def test_template_only_is_neither_hit_nor_miss
    r = Beachcomber::Result.new(ok: true, data: nil, age_ms: 0, stale: false, error: nil, template: '{{ 1 }}')
    assert r.ok?
    refute r.hit?
    refute r.miss?
    assert_equal '{{ 1 }}', r.template
    assert_nil r.rest
  end

  def test_template_and_rest_default_to_nil
    r = make_result(data: 'main')
    assert_nil r.template
    assert_nil r.rest
  end

  # --- stale? ---

  def test_stale_false_by_default
    r = make_result(data: 'x')
    refute r.stale?
  end

  def test_stale_true
    r = make_result(data: 'x', stale: true)
    assert r.stale?
  end

  # --- age_ms ---

  def test_age_ms
    r = make_result(data: 'v', age_ms: 42)
    assert_equal 42, r.age_ms
  end

  # --- data ---

  def test_data_string
    r = make_result(data: 'main')
    assert_equal 'main', r.data
  end

  def test_data_hash
    r = make_result(data: { 'branch' => 'main', 'dirty' => false })
    assert_equal 'main', r.data['branch']
    assert_equal false,  r.data['dirty']
  end

  def test_data_integer
    r = make_result(data: 99)
    assert_equal 99, r.data
  end

  def test_data_array
    r = make_result(data: [1, 2, 3])
    assert_equal [1, 2, 3], r.data
  end

  # --- [] accessor ---

  def test_bracket_access_on_hash_data
    r = make_result(data: { 'branch' => 'main', 'dirty' => true })
    assert_equal 'main', r['branch']
    assert_equal true,   r['dirty']
  end

  def test_bracket_access_missing_key_returns_nil
    r = make_result(data: { 'branch' => 'main' })
    assert_nil r['nonexistent']
  end

  def test_bracket_raises_on_non_hash_data
    r = make_result(data: 'a string')
    assert_raises(TypeError) { r['field'] }
  end

  def test_bracket_raises_on_nil_data
    r = make_result
    assert_raises(TypeError) { r['field'] }
  end

  # --- inspect ---

  def test_inspect_includes_class_name
    r = make_result(data: 'main', age_ms: 5)
    assert_match(/Beachcomber::Result/, r.inspect)
    assert_match(/data=/, r.inspect)
    assert_match(/age_ms=5/, r.inspect)
    refute_match(/template=/, r.inspect)
  end

  def test_inspect_shows_the_template_half
    r = Beachcomber::Result.new(ok: true, data: nil, age_ms: 0, stale: false, error: nil, template: '{{ 1 }}', rest: 'x')
    assert_match(/template="\{\{ 1 \}\}"/, r.inspect)
    assert_match(/rest="x"/, r.inspect)
  end
end
