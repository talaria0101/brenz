/**
 * @file Call of Duty (2003) GSC script parser
 * @author Kazam <kazam0180@proton.me>
 * @license GNU GPLv3
 */

/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

function toCaseInsensitive(a) {
  var ca = a.charCodeAt(0);
  if (ca>=97 && ca<=122) return `[${a}${a.toUpperCase()}]`;
  if (ca>=65 && ca<= 90) return `[${a.toLowerCase()}${a}]`;
  return a;
}

function caseInsensitive (keyword) {
  return new RegExp(keyword
  .split('')
  .map(toCaseInsensitive)
  .join('')
  )
}

module.exports = grammar({
  name: "gsc",

  extras: $ => [
    /\s/,
    $.comment,
  ],

  rules: {
    source_file: $ => repeat($._top_level),

    _top_level: $ => choice(
      $.function_definition,
      $.variable_declaration,
      $.comment
    ),

    // Function definitions
    function_definition: $ => seq(
      field('func_head', $.func_head),
      field('func_block', $.block)
    ),

    func_head: $ => seq(
      $.identifier,
      '(',
      optional($.parameter_list),
      ')'
    ),

    parameter_list: $ => seq(
      $.identifier,
      repeat(seq(',', $.identifier))
    ),

    // Variable declarations
    variable_declaration: $ => prec(1, seq(
      $.identifier,
      '=',
      $.expression,
      ';'
    )),

    // Statements
    statement: $ => choice(
      $.expression_statement,
      $.if_statement,
      $.while_statement,
      $.for_statement,
      $.foreach_statement,
      $.switch_statement,
      $.return_statement,
      $.break_statement,
      $.continue_statement,
      $.wait_statement,
      $.block
    ),

    expression_statement: $ => prec(-1, seq($.expression, ';')),

    if_statement: $ => prec.right(seq(
      'if',
      '(',
      field('condition', $.expression),
      ')',
      field('consequence', $.statement),
      optional(seq(
        'else',
        field('alternative', $.statement)
      ))
    )),

    while_statement: $ => seq(
      'while',
      '(',
      field('loop_condition', $.expression),
      ')',
      field('loop_block', $.statement)
    ),

    for_statement: $ => seq(
      'for',
      '(',
      field('loop_condition', choice(
        $.infinite_for,
        $.finite_for,
      )),

      ')',
      field('loop_block', $.statement)
    ),

    infinite_for: $ => ';;',
    finite_for: $ => seq(
      field('init', $.expression),
      ';',
      field('condition', $.expression),
      ';',
      field('update', $.expression),
    ),

    foreach_statement: $ => seq(
      'foreach',
      '(',
      field('needle', $.identifier),
      'in',
      field('hay_stack', $.expression),
      ')',
      $.statement
    ),

    switch_statement: $ => seq(
      'switch',
      '(',
      field('tested', $.expression),
      ')',
      field('switch_block', seq(
        '{',
        repeat(choice($.case_statement, $.default_statement)),
        '}'
      ))
    ),

    case_statement: $ => seq(
      'case',
      field('possible_value', $.expression),
      ':',
      repeat($.statement)
    ),

    default_statement: $ => seq(
      'default',
      ':',
      repeat($.statement)
    ),

    return_statement: $ => seq(
      'return',
      optional(field('returned', $.expression)),
      ';'
    ),

    break_statement: $ => seq('break', ';'),
    continue_statement: $ => seq('continue', ';'),

    wait_statement: $ => seq('wait', field('duration', $.expression), ';'),

    block: $ => seq(
      '{',
      repeat($.statement),
      '}'
    ),

    // Expressions
    expression: $ => choice(
      $.binary_expression,
      $.unary_expression,
      $.call_expression,
      $.function_pointer,
      $.member_expression,
      $.array,
      $.array_access,
      $.assignment_expression,
      $.postfix_expression,
      $.cast_expression,
      $.identifier,
      $.number,
      $.string,
      $.lstring,
      $.boolean,
      $.undefined,
      $.vec1,
      $.vec3
    ),

    binary_expression: $ => choice(
      prec.left(1, seq($.expression, '||', $.expression)),
      prec.left(2, seq($.expression, '&&', $.expression)),
      prec.left(3, seq($.expression, '|', $.expression)),
      prec.left(4, seq($.expression, '^', $.expression)),
      prec.left(5, seq($.expression, '&', $.expression)),
      prec.left(6, seq($.expression, choice('==', '!='), $.expression)),
      prec.left(7, seq($.expression, choice('<', '>', '<=', '>='), $.expression)),
      prec.left(8, seq($.expression, choice('<<', '>>'), $.expression)),
      prec.left(9, seq($.expression, choice('+', '-'), $.expression)),
      prec.left(10, seq($.expression, choice('*', '/', '%'), $.expression))
    ),

    unary_expression: $ => prec(11, choice(
      seq('!', $.expression),
      seq('-', $.expression),
      seq('+', $.expression),
      seq('~', $.expression)
    )),

    postfix_expression: $ => prec(12, seq(
      choice($.identifier, $.member_expression, $.array_access),
      choice('++', '--')
    )),

    assignment_expression: $ => prec.right(0, seq(
      field('variable', $.expression),
      choice('=', '+=', '-=', '*=', '/=', '%=', '&=', '|=', '^=', '<<=', '>>='),
      field('assigned_value', $.expression)
    )),

    call_expression: $ => choice(
      // Direct calls
      $.direct_call,
      // Thread calls
      $.thread_call,
      // Object method calls (self func())
      $.object_call,
      // Pointer calls (stored function references)
      $.pointer_call
    ),

    direct_call: $ => prec(2, seq(
      choice(
        $.foreign_function_ptr,
        field('function', $.identifier)
      ),
      '(',
      optional($.argument_list),
      ')'
    )),

    // Thread calls with 'thread' keyword without object
    thread_call: $ => prec(3, seq(
      'thread',
      choice(
        $.foreign_function_ptr,
        field('function', $.identifier),
        $.stored_func_ref
      ),
      '(',
      optional($.argument_list),
      ')'
    )),

    // Object method calls: obj func(), obj thread func(), obj thread script::func()
    object_call: $ => prec(2, seq(
      field('object', $._callable_object),
      optional('thread'),
      $._function_ref,
      '(',
      optional($.argument_list),
      ')'
    )),

    // Pointer calls: [[ref]](), obj [[ref]](), obj thread [[ref]]()
    pointer_call: $ => prec(2, seq(
      optional(seq(field('object', $._callable_object), optional('thread'))),
      field('stored_func', $.stored_func_ref),
      '(',
      optional($.argument_list),
      ')'
    )),

    // Objects that can have methods called on them
    _callable_object: $ => choice(
      $.identifier,
      $.member_expression,
      $.array_access
    ),

    // Function references
    function_pointer: $ => choice(
      $.local_function_ptr,
      $.foreign_function_ptr
    ),

    // Local function pointer: ::func
    local_function_ptr: $ => seq(
      '::',
      field('function', $.identifier)
    ),

    foreign_function_ptr: $ => prec(1, seq(
      field('path', alias(repeat($.path_component), $.path)),
      field('script', $.identifier),
      '::',
      field('function', $.identifier)
    )),

    // Function references in calls
    _function_ref: $ => choice(
      $.foreign_function_ptr,    // path\script::func
      $.local_function_ptr,      // ::func
      field('function', $.identifier) // just func (for direct calls)
    ),

    path_component: $ => prec(2, seq($.identifier, '\\')),

    // Stored function reference: [[expr]]
    stored_func_ref: $ => seq(
      '[[',
      field('ref', choice(
        $.identifier,
        $.member_expression,
        $.array_access
      )),
      ']]'
    ),

    member_expression: $ => prec.left(13, seq(
      choice(
        field('object', choice($.identifier, $.array_access, $.vec1)),
        field('parent_member_expr', $.member_expression)
      ),
      '.',
      field('member', $.identifier)
    )),

    cast_expression: $ => seq(
      '(',
      field('type_name', choice(
        'int',
        'float',
        'bool',
        'string'
      )),
      ')',
      field('var', choice(
        $.number,
        $.string,
        $.boolean,
        $.identifier,
        $.array_access,
        $.vec1
      ))
    ),

    array_access: $ => prec.left(14, seq(
      choice($.identifier, $.member_expression),
      repeat1(seq('[', $.expression, ']'))
    )),

    argument_list: $ => seq(
      $.expression,
      repeat(seq(',', $.expression))
    ),

    vec1: $ => seq('(', $.expression, ')'),
    vec3: $ => seq(
      '(',
      $.expression,
      ',',
      $.expression,
      ',',
      $.expression,
      ')'
    ),

    // Literals
    identifier: $ => /[a-zA-Z_][a-zA-Z0-9_]*/i,

    number: $ => choice(
      /\d+/,
      /\d*\.\d+/,
      /0x[0-9a-fA-F]+/
    ),

    string: $ => seq('"', repeat(choice(/[^"\\]/, /\\./, seq('//', /.*/))), '"'),
    // localized string
    lstring: $ => seq('&', $.string),

    boolean: $ => choice('true', 'false'),
    undefined: $ => 'undefined',

    array: $ => '[]',

    // Comments
    // http://stackoverflow.com/questions/13014947/regex-to-match-a-c-style-multiline-comment/36328890#36328890
    comment: _ => token(choice(
      seq('//', /(\\+(.|\r?\n)|[^\\\n])*/),
      seq('/*', /[^*]*\*+([^/*][^*]*\*+)*/, '/'),
    )),
  }
});
